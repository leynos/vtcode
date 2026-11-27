//! ACP session state and durable archive lifecycle. ACP handlers delegate
//! workspace-aware list/load/resume operations here, then provider runtime
//! uses the resulting session handles for model execution.
//!
//! The bootstrap that builds and registers a handle lives in the private
//! `bootstrap` child module so this file stays within the size budget. The
//! session-state test tree imports this module's namespace with `use super::*`,
//! so its prelude keeps the fixture types the tests name directly.

use super::{ZedAgent, log_suppressed_error};
use crate::acp;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use anyhow::Context;
use diapason_config::auth::AuthCredentialsStoreMode;
use diapason_core::core::threads::ThreadBootstrap;
use diapason_core::hooks::{LifecycleHookEngine, SessionStartTrigger};
use diapason_core::llm::provider::{FinishReason, Message};
use diapason_core::utils::session_archive::SessionArchive;
use futures::future::BoxFuture;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};

use super::super::session::AcpSessionWorkspace;
use super::super::types::{SessionHandle, SessionPromptGuard, lock_poison_error};
use super::{SessionWorkspaceLease, SessionWorkspaceRuntime};
#[cfg(test)]
use metadata::BoundedMetadataSizeWriter;
use metadata::{
    ACP_SESSION_METADATA_NAMESPACE, MAX_ACP_METADATA_ENTRIES, MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
    SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, merge_bounded_acp_meta, migrate_legacy_acp_meta,
};

mod archive;
mod bootstrap;
mod configuration;
mod lifecycle;
mod metadata;

pub(in crate::zed::agent) use archive::{CheckpointFailure, RequiredCheckpointError, checkpoint_failure_label};
pub(super) use bootstrap::SessionHandleBootstrap;

pub(super) fn safe_internal_error(
    operation: &'static str,
    error_category: &'static str,
    message: &'static str,
) -> acp::Error {
    tracing::error!(operation, error_category, "ACP request failed");
    acp::Error::internal_error().data(serde_json::json!({
        "operation": operation,
        "error_category": error_category,
        "message": message,
    }))
}

pub(super) fn safe_invalid_params_error(
    operation: &'static str,
    error_category: &'static str,
    message: &'static str,
) -> acp::Error {
    tracing::warn!(operation, error_category, "ACP request rejected");
    acp::Error::invalid_params().data(serde_json::json!({
        "operation": operation,
        "error_category": error_category,
        "message": message,
    }))
}

impl ZedAgent {
    const SESSION_LIST_PAGE_SIZE: usize = 100;

    pub(super) fn merge_session_acp_meta(
        &self,
        session: &SessionHandle,
        acp_meta: Option<acp::Meta>,
    ) -> anyhow::Result<()> {
        debug!(acp_meta_key_count = acp_meta.as_ref().map_or(0, acp::Meta::len), "Received ACP request metadata");
        let data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("merging ACP client metadata into the session archive", error))?;
        let Some(mut metadata) = data.thread.metadata() else {
            return Ok(());
        };
        let existing = metadata
            .extension(ACP_SESSION_METADATA_NAMESPACE)
            .and_then(serde_json::Value::as_object)
            .cloned();
        let merged = merge_bounded_acp_meta(existing, acp_meta);
        if let Some(value) = merged.value {
            metadata.set_extension(ACP_SESSION_METADATA_NAMESPACE, serde_json::Value::Object(value));
        }
        data.thread.replace_metadata(Some(metadata));
        if merged.discarded_entries > 0 {
            warn!(
                discarded_entries = merged.discarded_entries,
                max_entries = MAX_ACP_METADATA_ENTRIES,
                max_serialized_bytes = MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
                "Discarded ACP metadata entries beyond the archive budget"
            );
        }
        Ok(())
    }

    pub(crate) fn session_handle(&self, session_id: &acp::SessionId) -> anyhow::Result<Option<SessionHandle>> {
        Ok(self
            .sessions
            .lock()
            .map_err(|error| lock_poison_error("looking up ACP session", error))?
            .get(session_id)
            .cloned())
    }

    pub(super) fn begin_session_prompt(&self, session_id: &acp::SessionId) -> Result<SessionPromptGuard, acp::Error> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|error| acp::Error::internal_error().data(format!("Failed to admit ACP prompt: {error}")))?;
        let session = sessions
            .get(session_id)
            .ok_or_else(|| acp::Error::invalid_params().data(serde_json::json!({ "reason": "unknown_session" })))?;
        session
            .try_start_prompt()
            .ok_or_else(|| acp::Error::invalid_params().data(serde_json::json!({ "reason": "session_closing" })))
    }

    pub(super) fn push_message(&self, session: &SessionHandle, message: Message) -> anyhow::Result<()> {
        let data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("appending a message to the ACP session", error))?;
        data.thread.append_message(message);
        Ok(())
    }

    pub(super) fn resolved_messages(&self, session: &SessionHandle) -> anyhow::Result<Vec<Message>> {
        let workspace_runtime = session
            .workspace_runtime()
            .context("reading ACP session workspace runtime while resolving messages")?;
        let system_prompt = workspace_runtime
            .as_ref()
            .map_or(self.system_prompt.as_str(), |runtime| runtime.system_prompt.as_str());
        let mut messages = Vec::with_capacity(10);
        if !system_prompt.trim().is_empty() {
            messages.push(Message::system(system_prompt.to_string()));
        }

        let history = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session messages", error))?;
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        if let Some(prompt) = primary_agents.prompt(&history.primary_agent) {
            messages.push(Message::system(prompt.to_string()));
        }
        messages.extend(history.thread.messages());
        Ok(messages)
    }

    pub(super) fn stop_reason_from_finish(finish: FinishReason) -> acp::StopReason {
        match finish {
            FinishReason::Stop | FinishReason::ToolCalls => acp::StopReason::EndTurn,
            FinishReason::Length => acp::StopReason::MaxTokens,
            FinishReason::ContentFilter | FinishReason::Refusal | FinishReason::Error(_) => acp::StopReason::Refusal,
            FinishReason::Pause => acp::StopReason::EndTurn,
        }
    }

    /// Programmatic equivalent of the SACP `session/new` handler — exposed
    /// for tests and for the SACP handler shim to call.
    /// The future is boxed because the session-open path is large enough to
    /// trip `clippy::large_futures` at every one of its call sites — the
    /// request handlers and the tests alike.
    pub(crate) fn new_session(
        &self,
        req: acp::NewSessionRequest,
    ) -> BoxFuture<'_, Result<acp::NewSessionResponse, acp::Error>> {
        Box::pin(async move {
            let workspace = self.workspace_policy.resolve_client_cwd(&req.cwd).await?;
            let desired_trust = self
                .workspace_runtime_config
                .zed_config
                .workspace_trust
                .to_workspace_trust_level();
            let _trust_outcome = self
                .workspace_trust_synchronizer
                .synchronize(workspace.path(), desired_trust)
                .await
                .map_err(|error| {
                    log_suppressed_error(&error, "new_session", "workspace_trust_sync_failed");
                    safe_internal_error(
                        "new_session",
                        "workspace_trust_sync_failed",
                        "Unable to synchronize workspace trust",
                    )
                })?;
            let workspace_lease = self.workspace_runtime_for(&workspace, None).await.map_err(|error| {
                log_suppressed_error(&error, "new_session", "workspace_runtime_setup_failed");
                safe_internal_error(
                    "new_session",
                    "workspace_runtime_setup_failed",
                    "Unable to initialize ACP workspace runtime",
                )
            })?;
            let session_id = self
                .register_durable_session(workspace_lease, req.meta)
                .await
                .map_err(|error| {
                    log_suppressed_error(&error, "new_session", "session_archive_initialization_failed");
                    safe_internal_error(
                        "new_session",
                        "session_archive_initialization_failed",
                        "Unable to initialize ACP session storage",
                    )
                })?;
            let session = match self.session_handle(&session_id) {
                Ok(Some(session)) => session,
                Ok(None) => {
                    self.discard_failed_new_session(&session_id).await;
                    return Err(acp::Error::internal_error().data("newly registered ACP session is missing"));
                }
                Err(error) => {
                    self.discard_failed_new_session(&session_id).await;
                    log_suppressed_error(&error, "new_session", "session_registration_failed");
                    return Err(safe_internal_error(
                        "new_session",
                        "session_registration_failed",
                        "Unable to register ACP session",
                    ));
                }
            };
            if let Err(error) = self.ensure_task_lifecycle_forwarder(&session) {
                self.discard_failed_new_session(&session_id).await;
                log_suppressed_error(&error, "new_session", "task_lifecycle_setup_failed");
                return Err(safe_internal_error(
                    "new_session",
                    "task_lifecycle_setup_failed",
                    "Unable to initialize ACP session updates",
                ));
            }
            if let Err(error) = self.run_session_start_hooks(&session).await {
                self.discard_failed_new_session(&session_id).await;
                log_suppressed_error(&error, "new_session", "session_start_hook_failed");
                return Err(safe_internal_error(
                    "new_session",
                    "session_start_hook_failed",
                    "Unable to run ACP session start hooks",
                ));
            }
            let config_options = match self.current_session_config_options(&session) {
                Ok(config_options) => config_options,
                Err(error) => {
                    self.discard_failed_new_session(&session_id).await;
                    log_suppressed_error(&error, "new_session", "session_configuration_failed");
                    return Err(safe_internal_error(
                        "new_session",
                        "session_configuration_failed",
                        "Unable to read ACP session configuration",
                    ));
                }
            };

            if let Err(error) = self.send_available_commands_update(&session_id).await {
                warn!(%error, "Failed to advertise initial slash commands");
            }

            debug!(%session_id, "Created ACP session");

            Ok(acp::NewSessionResponse::new(session_id).config_options(config_options))
        })
    }
}

#[cfg(test)]
#[path = "session_state_tests.rs"]
mod tests;
