//! Session bootstrap: building the sub-agent controller and session handle a
//! new or resumed session registers.
//!
//! A session is registered from a thread plus the workspace runtime it leases,
//! and the sub-agent controller is attached to that runtime's registry before
//! the handle exists. These builders stay in a private child module so the
//! session-state root keeps to the size budget while the registration path and
//! its ordering rules stay together.

use super::super::optional_setup::report_optional_setup_outcome;
use super::super::{SessionWorkspaceLease, SessionWorkspaceRuntime, ZedAgent};
use super::metadata::{ACP_SESSION_METADATA_NAMESPACE, migrate_legacy_acp_meta};
use crate::acp;
use crate::zed::constants::SESSION_PREFIX;
use crate::zed::types::{SessionActivity, SessionCancellation, SessionData, SessionHandle, lock_poison_error};
use diapason_core::core::threads::{ThreadBootstrap, build_thread_archive_metadata};
use diapason_core::hooks::{LifecycleHookEngine, SessionStartTrigger};
use diapason_core::utils::session_archive::SessionArchive;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use tracing::warn;
use uuid::Uuid;

/// The parts a session handle is assembled from.
///
/// Every field is a decision the caller already made — which controller the
/// session owns, which archive it resumes from, which workspace runtime it
/// leases — so the builder only resolves configuration and lifecycle hooks.
pub(in crate::zed::agent) struct SessionHandleBootstrap {
    pub(super) subagent_controller: Option<Arc<diapason_core::subagents::SubagentController>>,
    pub(super) session_id: acp::SessionId,
    pub(super) archive: Option<SessionArchive>,
    pub(super) trigger: SessionStartTrigger,
    pub(super) workspace_runtime: Option<Arc<SessionWorkspaceRuntime>>,
    pub(super) workspace_lease: Option<SessionWorkspaceLease>,
}

impl ZedAgent {
    pub(super) async fn build_session_subagent_controller(
        &self,
        runtime: &SessionWorkspaceRuntime,
        session_id: &acp::SessionId,
        defer_background_restore: bool,
    ) -> Option<Arc<diapason_core::subagents::SubagentController>> {
        self.build_session_subagent_controller_with_owner_migration(
            runtime,
            session_id,
            defer_background_restore,
            false,
        )
        .await
    }

    /// Builds a session controller, deciding separately whether its owner
    /// migration must be retried.
    ///
    /// `defer_background_restore` keeps the controller from restoring before a
    /// resumed attach's durability checkpoint. `owner_migration_failed` selects
    /// the migration retry, which is needed only when the resume-site migration
    /// failed; retrying after a successful migration would take the ledger's
    /// lock again for rows that already carry their owner.
    pub(super) async fn build_session_subagent_controller_with_owner_migration(
        &self,
        runtime: &SessionWorkspaceRuntime,
        session_id: &acp::SessionId,
        defer_background_restore: bool,
        owner_migration_failed: bool,
    ) -> Option<Arc<diapason_core::subagents::SubagentController>> {
        let registry = runtime.local_tool_registry.with_subagent_controller(None);
        let mut config = self.config.clone();
        config.workspace = runtime.workspace_root.clone();
        let outcome = self
            .workspace_setup_ports
            .attach_session_subagent_controller(super::super::AcpSubagentControllerSetup {
                registry: &registry,
                agent_config: &config,
                custom_providers: &self.workspace_runtime_config.custom_providers,
                vt_config: self.vt_config.as_deref(),
                owner_session_id: Some(session_id.0.as_ref()),
                workspace_gated: runtime.workspace_hooks_gated,
                defer_background_restore,
                owner_migration_failed,
            })
            .await;
        // Only the subagent controller was set up here. Reporting through the
        // two-component helper would invent an MCP outcome and log "MCP client
        // disabled by configuration" for every session, whatever the MCP
        // client's real state is.
        report_optional_setup_outcome("subagent controller", &outcome);
        registry.subagent_controller()
    }

    pub(super) fn build_session_handle(
        &self,
        thread: diapason_core::core::threads::ThreadRuntimeHandle,
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
                write_ahead_call_ids: Vec::new(),
                auto_compact_suppressed: 0,
                lifecycle_hooks,
                session_started: false,
                session_ended: false,
                task_lifecycle_forwarder: None,
                task_lifecycle_snapshot_publisher: None,
                task_lifecycle_registration: None,
            })),
            cancellation: SessionCancellation::default(),
            activity: SessionActivity::default(),
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

    pub(super) async fn register_durable_session(
        &self,
        workspace_lease: SessionWorkspaceLease,
        acp_meta: Option<acp::Meta>,
    ) -> anyhow::Result<acp::SessionId> {
        let workspace_runtime = Arc::clone(workspace_lease.runtime());
        let session_id = acp::SessionId::new(Arc::from(format!("{SESSION_PREFIX}-{}", Uuid::new_v4())));
        let subagent_controller = self
            .build_session_subagent_controller(&workspace_runtime, &session_id, false)
            .await;
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
}
