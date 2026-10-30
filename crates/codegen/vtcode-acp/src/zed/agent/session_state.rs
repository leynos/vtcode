//! ACP session state and durable archive lifecycle. ACP handlers delegate
//! workspace-aware list/load/resume operations here, then provider runtime
//! uses the resulting session handles for model execution.

use super::{ZedAgent, log_suppressed_error};
use crate::acp;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use anyhow::Context;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::core::threads::{ThreadBootstrap, build_thread_archive_metadata};
use vtcode_core::hooks::{LifecycleHookEngine, SessionStartTrigger};
use vtcode_core::llm::provider::{FinishReason, Message};
use vtcode_core::utils::session_archive::SessionArchive;

use super::super::constants::SESSION_PREFIX;
use super::super::session::AcpSessionWorkspace;
use super::super::types::{SessionData, SessionHandle, SessionPromptGuard, lock_poison_error};
use super::{SessionWorkspaceLease, SessionWorkspaceRuntime};
#[cfg(test)]
use metadata::BoundedMetadataSizeWriter;
use metadata::{
    ACP_SESSION_METADATA_NAMESPACE, MAX_ACP_METADATA_ENTRIES, MAX_SESSION_ARCHIVE_EXTENSION_BYTES,
    SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, merge_bounded_acp_meta, migrate_legacy_acp_meta,
};

mod archive;
mod configuration;
mod lifecycle;
mod metadata;

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

struct SessionHandleBootstrap {
    subagent_controller: Option<Arc<vtcode_core::subagents::SubagentController>>,
    session_id: acp::SessionId,
    archive: Option<SessionArchive>,
    trigger: SessionStartTrigger,
    workspace_runtime: Option<Arc<SessionWorkspaceRuntime>>,
    workspace_lease: Option<SessionWorkspaceLease>,
}

impl ZedAgent {
    async fn build_session_subagent_controller(
        &self,
        runtime: &SessionWorkspaceRuntime,
        session_id: &acp::SessionId,
    ) -> Option<Arc<vtcode_core::subagents::SubagentController>> {
        let registry = runtime.local_tool_registry.with_subagent_controller(None);
        let mut config = self.config.clone();
        config.workspace = runtime.workspace_root.clone();
        let outcome = self
            .workspace_setup_ports
            .attach_session_subagent_controller(super::AcpSubagentControllerSetup {
                registry: &registry,
                agent_config: &config,
                custom_providers: &self.workspace_runtime_config.custom_providers,
                vt_config: self.vt_config.as_deref(),
                owner_session_id: Some(session_id.0.as_ref()),
                workspace_gated: runtime.workspace_hooks_gated,
            })
            .await;
        super::report_optional_workspace_setup_outcomes(&super::OptionalWorkspaceSetupOutcomes {
            subagent_controller: outcome,
            mcp_client: super::OptionalSetupOutcome::DisabledByConfiguration,
        });
        registry.subagent_controller()
    }
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

    fn build_session_handle(
        &self,
        thread: vtcode_core::core::threads::ThreadRuntimeHandle,
        bootstrap: SessionHandleBootstrap,
    ) -> SessionHandle {
        let SessionHandleBootstrap {
            subagent_controller,
            session_id,
            mut archive,
            trigger,
            workspace_runtime,
            workspace_lease,
        } = bootstrap;
        // Archive metadata is protocol-neutral in core; restore the old ACP
        // fields before the client adapter exposes this session.
        if let Some(mut metadata) = thread.metadata()
            && migrate_legacy_acp_meta(&mut metadata)
        {
            if let Some(archive) = archive.as_mut() {
                archive.replace_metadata(metadata.clone());
            }
            thread.replace_metadata(Some(metadata));
        }
        let reasoning_effort = self.session_reasoning_effort_for_thread(&thread);
        let provider = self.session_provider_for_thread(&thread);
        let model = self.session_model_for_thread(&thread);
        let primary_agents = workspace_runtime
            .as_ref()
            .map_or(&self.primary_agents, |runtime| &runtime.primary_agents);
        let primary_agent = self.session_primary_agent_for_thread(&thread, primary_agents);
        let hook_workspace = workspace_runtime
            .as_ref()
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        let workspace_hooks_gated = workspace_runtime.as_ref().map_or_else(
            || {
                self.vt_config
                    .as_ref()
                    .and_then(|config| config.workspace_lifecycle_hooks.as_ref())
                    .is_some_and(|hooks| !hooks.is_empty())
            },
            |runtime| runtime.workspace_hooks_gated,
        );
        let lifecycle_hooks = self.vt_config.as_ref().and_then(|config| {
            match LifecycleHookEngine::new_with_session_gated(
                hook_workspace,
                &config.hooks,
                trigger,
                session_id.0.to_string(),
                workspace_hooks_gated,
            ) {
                Ok(hooks) => hooks,
                Err(error) => {
                    warn!(%error, "Failed to initialize ACP lifecycle hooks");
                    None
                }
            }
        });
        SessionHandle {
            data: Arc::new(Mutex::new(SessionData {
                subagent_controller,
                session_id,
                thread,
                archive,
                workspace_runtime,
                workspace_lease,
                primary_agent,
                reasoning_effort,
                provider,
                model,
                last_tool_call_at: None,
                auto_compact_suppressed: 0,
                lifecycle_hooks,
                session_started: false,
                session_ended: false,
                task_lifecycle_forwarder: None,
                task_lifecycle_registration: None,
            })),
            cancellation: super::super::types::SessionCancellation::default(),
            activity: super::super::types::SessionActivity::default(),
            prompt_gate: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub(crate) fn register_session(&self) -> anyhow::Result<acp::SessionId> {
        let raw_id = self.next_session_id.fetch_add(1, Ordering::Relaxed);
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{raw_id}")));
        let metadata = build_thread_archive_metadata(
            self.config.workspace.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        );
        let thread = self
            .thread_manager
            .start_thread_with_identifier(session_id.0.to_string(), ThreadBootstrap::new(Some(metadata)));
        let handle = self.build_session_handle(
            thread,
            SessionHandleBootstrap {
                session_id: session_id.clone(),
                archive: None,
                trigger: SessionStartTrigger::NewSession,
                workspace_runtime: None,
                subagent_controller: None,
                workspace_lease: None,
            },
        );
        drop(
            self.sessions
                .lock()
                .map_err(|error| lock_poison_error("registering ACP session", error))?
                .insert(session_id.clone(), handle),
        );
        Ok(session_id)
    }

    async fn register_durable_session(
        &self,
        workspace_lease: SessionWorkspaceLease,
        acp_meta: Option<acp::Meta>,
    ) -> anyhow::Result<acp::SessionId> {
        let workspace_runtime = Arc::clone(workspace_lease.runtime());
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{}", Uuid::new_v4())));
        let subagent_controller = self.build_session_subagent_controller(&workspace_runtime, &session_id).await;
        let mut metadata = build_thread_archive_metadata(
            workspace_runtime.workspace_root.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        )
        .with_primary_agent(workspace_runtime.primary_agents.default_id());
        metadata.set_extension(ACP_SESSION_METADATA_NAMESPACE, serde_json::json!({}));
        let thread = self
            .thread_manager
            .start_thread_with_identifier(session_id.0.to_string(), ThreadBootstrap::new(Some(metadata)));
        let handle = self.build_session_handle(
            thread,
            SessionHandleBootstrap {
                session_id: session_id.clone(),
                archive: None,
                trigger: SessionStartTrigger::NewSession,
                workspace_runtime: Some(workspace_runtime),
                subagent_controller,
                workspace_lease: Some(workspace_lease),
            },
        );
        if let Err(error) = self.merge_session_acp_meta(&handle, acp_meta) {
            if let Err(cleanup_error) = handle.release_workspace_lease().await {
                warn!(
                    %cleanup_error,
                    session_id = %session_id,
                    "Failed to release ACP workspace lease after session metadata merge failed"
                );
            }
            return Err(error);
        }
        let insert_result = self
            .sessions
            .lock()
            .map_err(|error| lock_poison_error("registering durable ACP session", error))
            .map(|mut sessions| drop(sessions.insert(session_id.clone(), handle.clone())));
        if let Err(error) = insert_result {
            if let Err(cleanup_error) = handle.release_workspace_lease().await {
                warn!(
                    %cleanup_error,
                    session_id = %session_id,
                    "Failed to release ACP workspace lease after session registration failed"
                );
            }
            return Err(error);
        }
        Ok(session_id)
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
    pub(crate) async fn new_session(&self, req: acp::NewSessionRequest) -> Result<acp::NewSessionResponse, acp::Error> {
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
        let workspace_lease = self.workspace_runtime_for(&workspace).await.map_err(|error| {
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
    }
}

#[cfg(test)]
#[path = "session_state_tests.rs"]
mod tests;
