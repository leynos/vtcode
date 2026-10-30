//! SACP `AgentToClient` handler registration for `ZedAgent`.
//!
//! The bridge translates SACP request/notification handlers into calls on
//! the existing [`ZedAgent`] methods. The methods themselves are mostly
//! preserved from the pre-1.0.0 trait-based implementation; the only thing
//! that changed is the wiring layer (this file) and the connection storage
//! ([`crate::zed::connection::ConnectionHandle`]).
//!
//! ## Spawn pattern
//!
//! [`acp::PromptRequest`] is the one handler that needs to
//! drive the SACP event loop forward *while* it runs. Per the SACP docs we
//! therefore use [`ConnectionTo::spawn`] from inside the handler closure,
//! and the actual prompt logic runs in the spawned task. The spawned task
//! has full access to `block_task()` and the agent's client-side helpers.
//!
//! All other handlers are quick and can be served synchronously without
//! `spawn`.
//!
//! ## Connection access
//!
//! The canonical `ConnectionHandle` is stashed in
//! [`crate::register_acp_connection`] by `run_acp_agent` before the
//! SACP event loop starts. Handlers reach the same handle through the
//! agent's `self.client()` accessor — they must **not** re-wrap the
//! per-handler `cx` into a new `ConnectionHandle` or they will race
//! each other on the global `Mutex<Option<Arc<ConnectionHandle>>>`
//! inside the agent.

use super::super::constants::*;
use super::super::helpers::text_chunk;
use super::super::types::{SessionHandle, ToolRuntime};
use super::ZedAgent;
use super::compaction::CompactionRequest;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::tooling::SupportedTool;
use agent_client_protocol::schema::v1::{CancelNotification, PromptRequest, PromptResponse};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, Dispatch, HandleDispatchFrom, Handled, Responder, RunWithConnectionTo,
    on_receive_notification, on_receive_request,
};
#[cfg(test)]
use client::advertised_agent_capabilities;
use client::register_client_handlers;
use futures::StreamExt;
use serde_json::json;
use session::{handle_cancel, register_session_config_handler, register_session_handlers};
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tracing::{Instrument, Span, debug, info, warn};
use vtcode_commons::ansi::strip_ansi_codes;
use vtcode_core::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::factory::ProviderConfig;
use vtcode_core::llm::factory::create_provider_with_config;
use vtcode_core::llm::provider::{
    LLMError, LLMErrorMetadata, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent, Message,
    ToolCall as LLMToolCall, ToolDefinition,
};
use vtcode_core::retry::{RetryDecision, RetryPolicyCoreExt};

use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderDeadlinePolicy, ProviderRequestRuntime};

mod client;
mod prompt;
mod provider;
mod session;
#[cfg(test)]
mod tests;

/// Register every SACP `AgentToClient` request/notification handler that the
/// vtcode bridge implements. The agent must be `Send + Sync + 'static` so
/// that the SACP `Builder` can move the handlers onto its background task.
pub fn install_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let builder = register_client_handlers(builder, Arc::clone(&agent));
    let builder = register_session_handlers(builder, Arc::clone(&agent));
    let builder = register_session_config_handler(builder, Arc::clone(&agent));
    register_prompt_handlers(builder, Arc::clone(&agent)).with_handler(LodySubagentManagementHandler { agent })
}

fn register_prompt_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let prompt_agent = Arc::clone(&agent);
    let notification_agent = Arc::clone(&agent);
    builder
        .on_receive_request(
            async move |req: PromptRequest, request_cx: Responder<PromptResponse>, cx: ConnectionTo<Client>| {
                handle_prompt(Arc::clone(&prompt_agent), req, request_cx, cx).await
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            {
                let agent = Arc::clone(&notification_agent);
                move |notif: CancelNotification, _cx: ConnectionTo<Client>| {
                    let agent = Arc::clone(&agent);
                    async move { handle_cancel(agent, notif).await }
                }
            },
            on_receive_notification!(),
        )
}

async fn handle_prompt(
    agent: Arc<ZedAgent>,
    req: PromptRequest,
    request_cx: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), SdkError> {
    let prompt_guard = agent.begin_session_prompt(&req.session_id)?;
    // The prompt handler drives several SACP RPCs (`fs/read_text_file`,
    // `terminal/create`, `session/request_permission`) from inside the
    // prompt loop. Those would deadlock if called directly on the
    // dispatch loop's task, so we spawn the work onto a child task that
    // is allowed to use `block_task()`. The canonical `ConnectionHandle`
    // was registered globally by `run_acp_agent` — we do not re-wrap
    // `cx` into a new handle here, that would race with concurrent
    // prompts on the agent's internal `Mutex<Option<Arc<ConnectionHandle>>>`.
    cx.spawn({
        let agent = Arc::clone(&agent);
        async move {
            let result = Box::pin(prompt::run_prompt(agent, req)).await;
            let result = if prompt_guard.is_closing() && result.is_err() {
                Ok(PromptResponse::new(acp::StopReason::Cancelled))
            } else {
                result
            };
            if let Err(error) = &result {
                warn!(%error, "ACP prompt failed");
            }
            if let Err(error) = request_cx.respond_with_result(result) {
                warn!(%error, "Failed to send prompt response");
            }
            Ok(())
        }
    })
    .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
    Ok(())
}

struct LodySubagentManagementHandler {
    agent: Arc<ZedAgent>,
}

impl HandleDispatchFrom<Client> for LodySubagentManagementHandler {
    async fn handle_dispatch_from(
        &mut self,
        message: Dispatch,
        connection: ConnectionTo<Client>,
    ) -> Result<Handled<Dispatch>, SdkError> {
        let Dispatch::Request(request, responder) = message else {
            return Ok(Handled::No { message, retry: false });
        };
        if !super::lody_management::is_lody_subagent_management_method(request.method()) {
            return Ok(Handled::No {
                message: Dispatch::Request(request, responder),
                retry: false,
            });
        }

        let (method, params) = request.into_parts();
        let agent = Arc::clone(&self.agent);
        connection
            .spawn(async move {
                let result = super::lody_management::handle_lody_subagent_management(&agent, &method, params).await;
                responder.respond_with_result(result)
            })
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        Ok(Handled::Yes)
    }

    fn describe_chain(&self) -> impl std::fmt::Debug {
        "LodySubagentManagementHandler"
    }
}
