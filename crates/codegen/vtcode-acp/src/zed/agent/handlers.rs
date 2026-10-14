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
use super::super::helpers::{agent_implementation_info, text_chunk};
use super::super::types::{SessionHandle, ToolRuntime};
use super::ZedAgent;
use super::compaction::CompactionRequest;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::tooling::SupportedTool;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest, ResumeSessionResponse,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_notification,
    on_receive_request,
};
use futures::StreamExt;
use serde_json::json;
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

mod prompt;
mod provider;
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
    register_prompt_handlers(builder, agent)
}

fn register_client_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let initialize_agent = Arc::clone(&agent);
    let authenticate_agent = Arc::clone(&agent);
    builder
        .on_receive_request(
            async move |req: InitializeRequest,
                        request_cx: Responder<InitializeResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_initialize(Arc::clone(&initialize_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: AuthenticateRequest,
                        request_cx: Responder<AuthenticateResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_authenticate(Arc::clone(&authenticate_agent), request_cx).await
            },
            on_receive_request!(),
        )
}

fn register_session_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    let new_session_agent = Arc::clone(&agent);
    let load_session_agent = Arc::clone(&agent);
    let list_sessions_agent = Arc::clone(&agent);
    let resume_session_agent = Arc::clone(&agent);
    builder
        .on_receive_request(
            async move |req: NewSessionRequest,
                        request_cx: Responder<NewSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_new_session(Arc::clone(&new_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: LoadSessionRequest,
                        request_cx: Responder<LoadSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_load_session(Arc::clone(&load_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ListSessionsRequest,
                        request_cx: Responder<ListSessionsResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_list_sessions(Arc::clone(&list_sessions_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
        .on_receive_request(
            async move |req: ResumeSessionRequest,
                        request_cx: Responder<ResumeSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                handle_resume_session(Arc::clone(&resume_session_agent), req, request_cx).await
            },
            on_receive_request!(),
        )
}

fn register_session_config_handler<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    builder.on_receive_request(
        async move |req: SetSessionConfigOptionRequest,
                    request_cx: Responder<SetSessionConfigOptionResponse>,
                    _cx: ConnectionTo<Client>| {
            handle_set_session_config_option(Arc::clone(&agent), req, request_cx).await
        },
        on_receive_request!(),
    )
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

async fn handle_initialize(
    agent: Arc<ZedAgent>,
    args: InitializeRequest,
    request_cx: Responder<InitializeResponse>,
) -> Result<(), SdkError> {
    let caps = args.client_capabilities.clone();
    *agent.client_capabilities.lock().map_err(|error| {
        SdkError::internal_error().data(format!("Failed to store ACP client capabilities: {error}"))
    })? = Some(caps);
    if args.protocol_version != acp::ProtocolVersion::V1 {
        warn!(
            requested = %args.protocol_version,
            "{}",
            INITIALIZE_VERSION_MISMATCH_LOG
        );
    }
    let mut capabilities = advertised_agent_capabilities();
    capabilities.prompt_capabilities.embedded_context = true;
    capabilities.prompt_capabilities.image = true;
    capabilities.prompt_capabilities.audio = true;
    capabilities.mcp_capabilities.http = true;
    capabilities.mcp_capabilities.sse = false;

    let auth_methods = build_auth_methods();
    let response = InitializeResponse::new(acp::ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(agent_implementation_info(agent.title()))
        .auth_methods(auth_methods);
    request_cx.respond(response)
}

fn advertised_agent_capabilities() -> acp::AgentCapabilities {
    let mut capabilities = acp::AgentCapabilities::default();
    capabilities.load_session = true;
    capabilities.session_capabilities = acp::SessionCapabilities::new()
        .list(acp::SessionListCapabilities::new())
        .resume(acp::SessionResumeCapabilities::new());
    capabilities
}

fn build_auth_methods() -> Vec<acp::AuthMethod> {
    let mut methods = vec![
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openai", "OpenAI OAuth")
                .description("Authenticate with OpenAI via OAuth 2.0 with PKCE"),
        ),
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openrouter", "OpenRouter OAuth")
                .description("Authenticate with OpenRouter via OAuth 2.0 with PKCE"),
        ),
    ];
    methods.push(acp::AuthMethod::Terminal(
        acp::AuthMethodTerminal::new("terminal-login", "Terminal Login")
            .description("Interactive terminal-based authentication via vtcode login command")
            .args(vec!["login".to_string()]),
    ));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new("env-api-keys", "API Key", env_api_keys())));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new(
        "env-base-urls",
        "API Base URL",
        env_base_urls(),
    )));
    methods
}

fn env_api_keys() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str, bool)] = &[
        ("OPENAI_API_KEY", "OpenAI", false),
        ("ANTHROPIC_API_KEY", "Anthropic", false),
        ("GEMINI_API_KEY", "Google Gemini", false),
        ("OPENROUTER_API_KEY", "OpenRouter", false),
        ("DEEPSEEK_API_KEY", "DeepSeek", false),
        ("META_API_KEY", "Meta AI", false),
        ("MODEL_API_KEY", "Meta AI (documented)", false),
        ("ZAI_API_KEY", "Z.AI", false),
        ("MOONSHOT_API_KEY", "Moonshot", false),
        ("MINIMAX_API_KEY", "MiniMax", false),
        ("GROQ_API_KEY", "Groq", false),
        ("XAI_API_KEY", "xAI", false),
        ("COHERE_API_KEY", "Cohere", false),
        ("HF_TOKEN", "Hugging Face", false),
        ("MISTRAL_API_KEY", "Mistral", false),
        ("GOOGLE_API_KEY", "Google (alt)", true),
        ("OLLAMA_API_KEY", "Ollama", true),
        ("OLLAMA_API_KEY", "Ollama Cloud", false),
        ("LMSTUDIO_API_KEY", "LM Studio", true),
    ];
    ENTRIES
        .iter()
        .map(|(name, label, optional)| {
            let mut var = acp::AuthEnvVar::new(*name).label(*label);
            if *optional {
                var = var.optional(true);
            }
            var
        })
        .collect()
}

fn env_base_urls() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str)] = &[
        ("OPENAI_BASE_URL", "OpenAI"),
        ("ANTHROPIC_BASE_URL", "Anthropic"),
        ("GEMINI_BASE_URL", "Gemini"),
        ("OPENROUTER_BASE_URL", "OpenRouter"),
        ("DEEPSEEK_BASE_URL", "DeepSeek"),
        ("META_BASE_URL", "Meta AI"),
        ("ZAI_BASE_URL", "Z.AI"),
        ("MOONSHOT_BASE_URL", "Moonshot"),
        ("MINIMAX_BASE_URL", "MiniMax"),
        ("XAI_BASE_URL", "xAI"),
        ("HUGGINGFACE_BASE_URL", "Hugging Face"),
        ("OLLAMA_BASE_URL", "Ollama"),
        ("LMSTUDIO_BASE_URL", "LM Studio"),
    ];
    ENTRIES
        .iter()
        .map(|(name, label)| acp::AuthEnvVar::new(*name).label(*label).optional(true))
        .collect()
}

async fn handle_authenticate(
    _agent: Arc<ZedAgent>,
    request_cx: Responder<AuthenticateResponse>,
) -> Result<(), SdkError> {
    request_cx.respond(AuthenticateResponse::default())
}

async fn handle_new_session(
    agent: Arc<ZedAgent>,
    req: NewSessionRequest,
    request_cx: Responder<NewSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.new_session(req).await?;
    request_cx.respond(response)
}

async fn handle_load_session(
    agent: Arc<ZedAgent>,
    req: LoadSessionRequest,
    request_cx: Responder<LoadSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.load_session(req).await?;
    request_cx.respond(response)
}

async fn handle_list_sessions(
    agent: Arc<ZedAgent>,
    req: ListSessionsRequest,
    request_cx: Responder<ListSessionsResponse>,
) -> Result<(), SdkError> {
    let response = agent.list_sessions(req).await?;
    request_cx.respond(response)
}

async fn handle_resume_session(
    agent: Arc<ZedAgent>,
    req: ResumeSessionRequest,
    request_cx: Responder<ResumeSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.resume_session(req).await?;
    request_cx.respond(response)
}

async fn handle_set_session_config_option(
    agent: Arc<ZedAgent>,
    req: SetSessionConfigOptionRequest,
    request_cx: Responder<SetSessionConfigOptionResponse>,
) -> Result<(), SdkError> {
    let response = agent.set_session_config_option(req).await?;
    request_cx.respond(response)
}

async fn handle_cancel(agent: Arc<ZedAgent>, notif: CancelNotification) -> Result<(), SdkError> {
    match agent.session_handle(&notif.session_id) {
        Ok(Some(session)) => session.cancellation.cancel(),
        Ok(None) => {}
        Err(error) => warn!(%error, session_id = %notif.session_id, "Failed to look up ACP session for cancellation"),
    }
    Ok(())
}

async fn handle_prompt(
    agent: Arc<ZedAgent>,
    req: PromptRequest,
    request_cx: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), SdkError> {
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
