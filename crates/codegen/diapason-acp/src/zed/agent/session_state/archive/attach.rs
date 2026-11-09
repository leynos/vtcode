//! Rebuilding a live session from an archived listing.
//!
//! A listing is repaired before it is handed out, checkpointed with required
//! durability when recovery changed it, and only then registered.
use anyhow::Context;
use diapason_core::core::threads::ThreadBootstrap;
use diapason_core::hooks::SessionStartTrigger;
use diapason_core::metrics::ToolRecoveryEvent;
use diapason_core::utils::session_archive::{
    SessionArchive, SessionListing, find_session_by_identifier, history_persistence_enabled,
};
use std::path::Path;
use tracing::{debug, warn};

use crate::acp;
use crate::zed::agent::log_suppressed_error;
use crate::zed::agent::session_state::{SessionHandleBootstrap, ZedAgent};
use crate::zed::session::AcpSessionWorkspace;
use crate::zed::types::{SessionHandle, lock_poison_error};

use super::{
    CheckpointDurability, SessionArchiveCheckpointReader, WorkspaceMatch, canonical_workspace_match,
    repair_archived_tool_calls, session_listing_workspace_match, trace_storage_operation,
};

impl ZedAgent {
    /// Rebuilds a thread from an archived listing and registers it.
    ///
    /// Interrupted tool calls are staged before the thread is handed out, and
    /// the repaired history is checkpointed when recovery changed it.
    async fn attach_thread_from_archive(
        &self,
        session_id: &acp::SessionId,
        identifier: &str,
        workspace: &AcpSessionWorkspace,
    ) -> anyhow::Result<SessionHandle> {
        trace_storage_operation("archive_attach", async {
            let resume_lock = self.resume_lock(session_id)?;
            let _resume_guard = resume_lock.lock().await;
            // Another attempt may have registered the session while this one
            // waited for the lock; reuse its result instead of repairing twice.
            if let Some(existing) = self.session_handle(session_id)? {
                return Ok(existing);
            }
            let listing = find_session_by_identifier(identifier)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown archived session '{identifier}'"))?;
            self.attach_thread_from_listing_locked(session_id, workspace, listing).await
        })
        .await
    }

    /// Attach an already-loaded archive listing as a live ACP session.
    ///
    /// The listing's own identifier names the session, so callers cannot
    /// attach a listing under a name that does not match its archive path.
    /// Concurrent attempts for the same session are serialized so only one
    /// repairs and registers it.
    pub(in crate::zed::agent::session_state) async fn attach_thread_from_listing(
        &self,
        session_id: &acp::SessionId,
        workspace: &AcpSessionWorkspace,
        listing: SessionListing,
    ) -> anyhow::Result<SessionHandle> {
        let resume_lock = self.resume_lock(session_id)?;
        let _resume_guard = resume_lock.lock().await;
        if let Some(existing) = self.session_handle(session_id)? {
            return Ok(existing);
        }
        self.attach_thread_from_listing_locked(session_id, workspace, listing).await
    }

    /// Attaches a listing while the caller already holds the per-session lock.
    async fn attach_thread_from_listing_locked(
        &self,
        session_id: &acp::SessionId,
        workspace: &AcpSessionWorkspace,
        mut listing: SessionListing,
    ) -> anyhow::Result<SessionHandle> {
        let identifier = listing.identifier();
        let identifier = identifier.as_str();
        let workspace_match = session_listing_workspace_match(&listing, workspace.path())
            .with_context(|| format!("resolving workspace ownership for archived session '{identifier}'"))?;
        anyhow::ensure!(
            workspace_match == WorkspaceMatch::Matches,
            "archived session '{identifier}' belongs to a different workspace"
        );
        let preserved_turn_diagnostics = listing
            .snapshot
            .progress
            .as_ref()
            .and_then(|progress| progress.turn_diagnostics.clone());
        let recovery = repair_archived_tool_calls(&mut listing).map_err(|error| {
            self.recovery_metrics().record_tool_recovery(ToolRecoveryEvent::AmbiguousHistory);
            warn!(session_id = %session_id, tool_recovery_outcome = "ambiguous_history_rejected",
                  "Rejected archived ACP session with ambiguous tool-result history");
            anyhow::anyhow!("Archived ACP session has ambiguous tool-result history and was not resumed; inspect the archive before continuing: {error}")
        })?;
        let archive = SessionArchive::resume_from_listing(&listing, listing.snapshot.metadata.clone());
        let thread = self
            .thread_manager
            .start_thread_with_identifier(identifier, ThreadBootstrap::from_listing(listing));
        let workspace_lease = self
            .workspace_runtime_for(workspace)
            .await
            .context("Failed to initialize archived ACP session workspace")?;
        let handle = self.build_session_handle(
            thread,
            SessionHandleBootstrap {
                session_id: session_id.clone(),
                archive: Some(archive),
                trigger: SessionStartTrigger::Resume,
                workspace_runtime: Some(workspace_lease.runtime().clone()),
                workspace_lease: Some(workspace_lease),
            },
        );
        if recovery.truncated_prefix > 0 {
            warn!(
                session_id = %session_id,
                truncated_results = recovery.truncated_prefix,
                "Archived ACP session prefix was truncated; leading tool results have no retained request"
            );
        }
        if recovery.history_changed() {
            let metrics = self.recovery_metrics();
            metrics.record_tool_recovery_events(ToolRecoveryEvent::InterruptedCall, recovery.repaired_calls);
            metrics.record_tool_recovery_events(ToolRecoveryEvent::RelocatedResult, recovery.relocated_calls);
            if let Err(error) = self
                .checkpoint_session_with_turn_diagnostics(
                    &handle,
                    preserved_turn_diagnostics,
                    CheckpointDurability::Required,
                    &recovery.evidence,
                    &SessionArchiveCheckpointReader,
                )
                .await
            {
                metrics.record_tool_recovery(ToolRecoveryEvent::CheckpointFailure);
                if let Err(cleanup_error) = handle.release_workspace_lease().await {
                    log_suppressed_error(&cleanup_error, "archive_attach", "workspace_lease_cleanup_failed");
                    warn!(
                        operation = "archive_attach",
                        error_category = "workspace_lease_cleanup_failed",
                        "Failed to release ACP workspace lease after repaired history checkpoint failed"
                    );
                }
                return Err(error).context("Failed to persist repaired ACP tool-call history");
            }
            metrics.record_tool_recovery(ToolRecoveryEvent::RepairedArchive);
            warn!(
                session_id = %session_id,
                repaired_calls = recovery.repaired_calls,
                relocated_calls = recovery.relocated_calls,
                "Repaired unresolved tool calls while loading durable ACP session"
            );
        }
        if let Some(existing) = self.register_resumed_session(session_id, &handle).await? {
            return Ok(existing);
        }
        Ok(handle)
    }

    /// Publish a resumed session, returning any handle registered concurrently.
    ///
    /// If another task registered the same session first, the existing handle
    /// wins and the newly built handle's workspace lease is released so a
    /// failed load leaves no half-owned runtime.
    async fn register_resumed_session(
        &self,
        session_id: &acp::SessionId,
        handle: &SessionHandle,
    ) -> anyhow::Result<Option<SessionHandle>> {
        let registration = self
            .sessions
            .lock()
            .map_err(|error| lock_poison_error("registering resumed ACP session", error))
            .map(|mut sessions| {
                if let Some(existing) = sessions.get(session_id) {
                    return Some(existing.clone());
                }
                let replaced = sessions.insert(session_id.clone(), handle.clone());
                debug_assert!(replaced.is_none(), "resumed session registration must not replace a handle");
                None
            });
        let existing = match registration {
            Ok(existing) => existing,
            Err(error) => {
                if let Err(cleanup_error) = handle.release_workspace_lease().await {
                    log_suppressed_error(&cleanup_error, "archive_attach", "workspace_lease_cleanup_failed");
                    warn!(
                        operation = "archive_attach",
                        error_category = "workspace_lease_cleanup_failed",
                        "Failed to release ACP workspace lease after resumed session registration failed"
                    );
                }
                return Err(error);
            }
        };
        if existing.is_some()
            && let Err(cleanup_error) = handle.release_workspace_lease().await
        {
            log_suppressed_error(&cleanup_error, "archive_attach", "workspace_lease_cleanup_failed");
            warn!(
                operation = "archive_attach",
                error_category = "workspace_lease_cleanup_failed",
                "Failed to release ACP workspace lease for a concurrently registered resumed session"
            );
        }
        Ok(existing)
    }

    pub(in crate::zed::agent::session_state) async fn attach_or_get_session(
        &self,
        session_id: &acp::SessionId,
        workspace: &AcpSessionWorkspace,
    ) -> anyhow::Result<SessionHandle> {
        if let Some(session) = self.session_handle(session_id)? {
            let stored_workspace = session
                .data
                .lock()
                .map_err(|error| lock_poison_error("checking workspace ownership of active ACP session", error))?
                .thread
                .metadata()
                .map(|metadata| metadata.workspace_path)
                .ok_or_else(|| anyhow::anyhow!("active session has no stored workspace path"))?;
            let workspace_match = canonical_workspace_match(Path::new(&stored_workspace), workspace.path())
                .with_context(|| format!("resolving workspace ownership for active session '{session_id}'"))?;
            anyhow::ensure!(
                workspace_match == WorkspaceMatch::Matches,
                "active session belongs to a different workspace"
            );
            return Ok(session);
        }

        anyhow::ensure!(history_persistence_enabled(), "durable session history is disabled");

        self.attach_thread_from_archive(session_id, session_id.0.as_ref(), workspace)
            .await
    }
}
