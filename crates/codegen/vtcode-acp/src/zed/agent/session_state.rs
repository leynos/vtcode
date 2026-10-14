//! ACP session state and durable archive lifecycle. ACP handlers delegate
//! workspace-aware list/load/resume operations here, then provider runtime
//! uses the resulting session handles for model execution.

use super::ZedAgent;
use crate::acp;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use anyhow::Context;
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::{debug, warn};
use uuid::Uuid;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_core::core::threads::{ThreadBootstrap, build_thread_archive_metadata};
use vtcode_core::hooks::{LifecycleHookEngine, SessionEndReason, SessionStartTrigger};
use vtcode_core::llm::provider::{FinishReason, Message};
use vtcode_core::utils::session_archive::{MAX_CLIENT_METADATA_BYTES, MAX_CLIENT_METADATA_ENTRIES, SessionArchive};

use super::super::constants::SESSION_PREFIX;
use super::super::session::AcpSessionWorkspace;
use super::super::types::{SessionData, SessionHandle, lock_poison_error};
use super::SessionWorkspaceRuntime;

mod archive;
mod configuration;

struct SessionHandleBootstrap {
    session_id: acp::SessionId,
    archive: Option<SessionArchive>,
    trigger: SessionStartTrigger,
    workspace_runtime: Option<Arc<SessionWorkspaceRuntime>>,
}

struct BoundedClientMetadata {
    value: Option<serde_json::Map<String, serde_json::Value>>,
    discarded_entries: usize,
}

fn merge_bounded_client_metadata(
    existing: Option<serde_json::Map<String, serde_json::Value>>,
    incoming: Option<acp::Meta>,
) -> BoundedClientMetadata {
    let preserve_presence = existing.is_some() || incoming.is_some();
    let mut merged = existing.unwrap_or_default().into_iter().collect::<BTreeMap<_, _>>();
    merged.extend(incoming.unwrap_or_default());

    let mut bounded = serde_json::Map::new();
    let mut discarded_entries = 0usize;
    for (key, value) in merged {
        if bounded.len() >= MAX_CLIENT_METADATA_ENTRIES {
            discarded_entries = discarded_entries.saturating_add(1);
            continue;
        }
        drop(bounded.insert(key.clone(), value));
        if serialized_acp_meta_size_at_most(&bounded).is_none() {
            drop(bounded.remove(&key));
            discarded_entries = discarded_entries.saturating_add(1);
        }
    }

    BoundedClientMetadata {
        value: preserve_presence.then_some(bounded),
        discarded_entries,
    }
}

impl ZedAgent {
    const SESSION_LIST_PAGE_SIZE: usize = 100;

    pub(crate) async fn run_session_start_hooks(&self, session: &SessionHandle) -> anyhow::Result<()> {
        if !session.mark_session_started().context("checking ACP session start state")? {
            return Ok(());
        }

        session
            .update_transcript_path()
            .await
            .context("updating ACP session transcript path")?;
        let Some(hooks) = session.lifecycle_hooks().context("reading ACP session lifecycle hooks")? else {
            return Ok(());
        };
        let workspace = session
            .workspace_runtime()
            .context("reading ACP session workspace runtime for SessionStart hooks")?
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        vtcode_core::hooks::restore_workspace_hook_approval(&hooks, &workspace).await;
        let outcome = hooks.run_session_start().await?;
        for message in outcome.messages {
            warn!(level = ?message.level, message = %message.text, "ACP SessionStart hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                self.push_message(session, Message::system(context))
                    .context("appending SessionStart hook context to the ACP session")?;
            }
        }
        Ok(())
    }

    pub(crate) async fn run_session_end_hooks(&self, reason: SessionEndReason) -> anyhow::Result<()> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|error| lock_poison_error("listing ACP sessions for SessionEnd hooks", error))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = Self::run_session_end_hooks_for(&session, reason).await {
                let _ = first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn run_session_end_hooks_for(session: &SessionHandle, reason: SessionEndReason) -> anyhow::Result<()> {
        let turn_id = match session.mark_session_ended() {
            Ok(Some(turn_id)) => turn_id,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(%error, "Failed to read ACP session end state; skipping lifecycle hooks");
                return Err(error.context("checking ACP session end state"));
            }
        };
        let hooks = match session.lifecycle_hooks() {
            Ok(Some(hooks)) => hooks,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(%error, "Failed to read ACP session lifecycle hooks at shutdown");
                return Err(error.context("reading ACP session lifecycle hooks at shutdown"));
            }
        };

        match hooks.run_session_end(&turn_id, reason).await {
            Ok(messages) => {
                for message in messages {
                    warn!(level = ?message.level, message = %message.text, "ACP SessionEnd hook");
                }
            }
            Err(error) => warn!(%error, "ACP SessionEnd hooks failed"),
        }
        Ok(())
    }

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
        let merged = merge_bounded_client_metadata(metadata.client_metadata.take(), acp_meta);
        metadata.client_metadata = merged.value;
        data.thread.replace_metadata(Some(metadata));
        if merged.discarded_entries > 0 {
            warn!(
                discarded_entries = merged.discarded_entries,
                max_entries = MAX_CLIENT_METADATA_ENTRIES,
                max_serialized_bytes = MAX_CLIENT_METADATA_BYTES,
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
        let SessionHandleBootstrap { session_id, archive, trigger, workspace_runtime } = bootstrap;
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
                session_id,
                thread,
                archive,
                workspace_runtime,
                tool_notice_sent: std::sync::atomic::AtomicBool::new(false),
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

    fn register_durable_session(
        &self,
        workspace_runtime: Arc<SessionWorkspaceRuntime>,
        acp_meta: Option<acp::Meta>,
    ) -> anyhow::Result<acp::SessionId> {
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{}", Uuid::new_v4())));
        let mut metadata = build_thread_archive_metadata(
            workspace_runtime.workspace_root.as_path(),
            &self.config.model,
            &self.config.provider,
            &self.config.theme,
            self.config.reasoning_effort.as_str(),
        )
        .with_primary_agent(workspace_runtime.primary_agents.default_id());
        metadata.client_metadata = Some(serde_json::Map::new());
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
            },
        );
        self.merge_session_acp_meta(&handle, acp_meta)?;
        drop(
            self.sessions
                .lock()
                .map_err(|error| lock_poison_error("registering durable ACP session", error))?
                .insert(session_id.clone(), handle),
        );
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

    pub(super) fn push_message(&self, session: &SessionHandle, message: Message) -> anyhow::Result<()> {
        let data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("appending a message to the ACP session", error))?;
        data.thread.append_message(message);
        Ok(())
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn should_send_tool_notice(&self, session: &SessionHandle) -> anyhow::Result<bool> {
        session
            .data
            .lock()
            .map_err(|error| lock_poison_error("checking whether to send the ACP tool notice", error))
            .map(|data| !data.tool_notice_sent.load(Ordering::Relaxed))
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(super) fn mark_tool_notice_sent(&self, session: &SessionHandle) -> anyhow::Result<()> {
        let data = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("marking the ACP tool notice as sent", error))?;
        data.tool_notice_sent.store(true, Ordering::Relaxed);
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
        let workspace = AcpSessionWorkspace::from_client_cwd(&req.cwd).await?;
        let desired_trust = self
            .workspace_runtime_config
            .zed_config
            .workspace_trust
            .to_workspace_trust_level();
        let _trust_outcome = self
            .workspace_trust_synchronizer
            .synchronize(workspace.path(), desired_trust)
            .await
            .map_err(|error| acp::Error::internal_error().data(format!("Failed to trust ACP session cwd: {error}")))?;
        let workspace_runtime = self
            .workspace_runtime_for(&workspace)
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        let session_id = self
            .register_durable_session(workspace_runtime, req.meta)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        let session = self
            .session_handle(&session_id)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?
            .ok_or_else(|| acp::Error::internal_error().data("newly registered ACP session is missing"))?;
        self.ensure_task_lifecycle_forwarder(&session)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        self.run_session_start_hooks(&session)
            .await
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;
        let config_options = self
            .current_session_config_options(&session)
            .map_err(|error| acp::Error::internal_error().data(error.to_string()))?;

        if let Err(error) = self.send_available_commands_update(&session_id).await {
            warn!(%error, "Failed to advertise initial slash commands");
        }

        debug!(%session_id, "Created ACP session");

        Ok(acp::NewSessionResponse::new(session_id).config_options(config_options))
    }
}

fn serialized_acp_meta_size_at_most(metadata: &serde_json::Map<String, serde_json::Value>) -> Option<usize> {
    let mut writer = BoundedMetadataSizeWriter { bytes_written: 0 };
    serde_json::to_writer(&mut writer, metadata).ok()?;
    Some(writer.bytes_written)
}

struct BoundedMetadataSizeWriter {
    bytes_written: usize,
}

impl Write for BoundedMetadataSizeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(new_size) = self.bytes_written.checked_add(bytes.len()) else {
            return Err(io::Error::other("serialized ACP metadata size overflowed"));
        };
        if new_size > MAX_CLIENT_METADATA_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "serialized ACP metadata exceeded its size limit"));
        }
        self.bytes_written = new_size;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
#[path = "session_state_tests.rs"]
mod tests;
