//! Durable archive persistence and workspace-bound session restoration.
//!
//! Archive recovery records unresolved tool effects as uncertain before a
//! session becomes active. Ambiguous terminal-result history is rejected
//! without replaying calls or registering a resumable session.
use anyhow::Context;
use diapason_core::core::threads::messages_from_session_listing;
use diapason_core::utils::session_archive::{
    SessionListing, SessionMessage, history_persistence_enabled, list_recent_sessions, session_workspace_path,
};
use futures::future::BoxFuture;
use std::future::Future;
use std::path::Path;
use std::time::Instant;
use tracing::{Instrument, debug, warn};

use crate::acp;
use crate::zed::session::AcpSessionWorkspace;

use super::{ZedAgent, safe_internal_error, safe_invalid_params_error};

mod attach;
mod checkpoint;
mod retention;

use crate::zed::agent::log_suppressed_error;
use crate::zed::agent::tool_recovery::{RecoveryReport, repair_unresolved_tool_calls};
pub(in crate::zed::agent) use checkpoint::{CheckpointFailure, RequiredCheckpointError};
pub(super) use retention::{
    CheckpointReader, SessionArchiveCheckpointReader, missing_persisted_tool_calls, missing_staged_batch_calls,
    required_checkpoint_retention_violation, required_staged_batch_violation, staged_tool_call_ids,
    verify_required_checkpoint_retention,
};

/// How strictly a checkpoint must reach durable storage.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum CheckpointDurability {
    /// A skipped or throttled checkpoint is logged and tolerated.
    BestEffort,
    /// The caller cannot proceed unless the snapshot was persisted.
    Required,
}

/// Runs one session-storage operation inside a bounded telemetry span.
///
/// The span records the fixed operation name, a success/failure outcome, and
/// elapsed milliseconds; it never records paths, session identifiers, or
/// payloads.
pub(super) fn trace_storage_operation<'a, T: 'a, E: 'a, F>(
    operation: &'static str,
    future: F,
) -> BoxFuture<'a, Result<T, E>>
where
    F: Future<Output = Result<T, E>> + Send + 'a,
{
    let span = tracing::info_span!(
        "acp_session_storage",
        operation,
        outcome = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    );
    let started_at = Instant::now();
    Box::pin(async move {
        let result = future.instrument(span.clone()).await;
        let _ = span.record("outcome", if result.is_ok() { "success" } else { "failure" });
        let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        let _ = span.record("elapsed_ms", elapsed_ms);
        result
    })
}

impl ZedAgent {
    fn session_list_offset(cursor: Option<&str>) -> Result<usize, acp::Error> {
        let Some(cursor) = cursor else {
            return Ok(0);
        };
        cursor
            .strip_prefix("offset:")
            .and_then(|offset| offset.parse().ok())
            .ok_or_else(|| acp::Error::invalid_params().data("invalid session list cursor"))
    }

    pub(super) fn session_list_response(
        args: &acp::ListSessionsRequest,
        listings: Vec<SessionListing>,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        let requested_workspace =
            args.cwd
                .as_deref()
                .map(diapason_commons::canonicalize)
                .transpose()
                .map_err(|error| {
                    log_suppressed_error(&error, "session_list", "workspace_resolution_failed");
                    safe_invalid_params_error(
                        "session_list",
                        "workspace_resolution_failed",
                        "Unable to resolve ACP session list workspace",
                    )
                })?;
        Self::session_list_response_for_workspace(args, listings, requested_workspace.as_deref())
    }

    fn session_list_response_for_workspace(
        args: &acp::ListSessionsRequest,
        listings: Vec<SessionListing>,
        requested_workspace: Option<&Path>,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        let offset = Self::session_list_offset(args.cursor.as_deref())?;
        let mut sessions = Vec::new();
        for listing in listings {
            if let Some(workspace) = requested_workspace {
                match session_listing_workspace_match(&listing, workspace) {
                    Ok(WorkspaceMatch::Matches) => {}
                    Ok(WorkspaceMatch::Differs) => continue,
                    Err(error) => {
                        log_suppressed_error(&error, "session_list", "stored_workspace_unresolvable");
                        debug!(
                            operation = "session_list",
                            error_category = "stored_workspace_unresolvable",
                            "Skipping archived ACP session with an unresolved workspace"
                        );
                        continue;
                    }
                }
            }

            let Some(cwd) = session_workspace_path(&listing) else {
                continue;
            };
            if cwd.is_absolute() {
                sessions.push(
                    acp::SessionInfo::new(listing.identifier(), cwd)
                        .title(listing.first_prompt_preview().or_else(|| listing.first_reply_preview()))
                        .updated_at(listing.snapshot.ended_at.to_rfc3339()),
                );
            }
        }
        let mut sessions = sessions
            .into_iter()
            .skip(offset)
            .take(Self::SESSION_LIST_PAGE_SIZE + 1)
            .collect::<Vec<_>>();

        let has_more = sessions.len() > Self::SESSION_LIST_PAGE_SIZE;
        sessions.truncate(Self::SESSION_LIST_PAGE_SIZE);
        let next_cursor = has_more.then(|| format!("offset:{}", offset + sessions.len()));
        Ok(acp::ListSessionsResponse::new(sessions).next_cursor(next_cursor))
    }

    /// Programmatic equivalent of the SACP `session/list` handler.
    pub(crate) async fn list_sessions(
        &self,
        args: acp::ListSessionsRequest,
    ) -> Result<acp::ListSessionsResponse, acp::Error> {
        trace_storage_operation("archive_list", async {
            let requested_workspace = match args.cwd.as_deref() {
                Some(cwd) => Some(self.workspace_policy.resolve_client_cwd(cwd).await?),
                None => None,
            };
            if !history_persistence_enabled() {
                return Ok(acp::ListSessionsResponse::new(Vec::new()));
            }

            let listings = list_recent_sessions(0).await.map_err(|error| {
                log_suppressed_error(&error, "session_list", "archive_read_failed");
                safe_internal_error("session_list", "archive_read_failed", "Unable to list archived ACP sessions")
            })?;
            let listing_span = tracing::Span::current();
            let response = tokio::task::spawn_blocking(move || {
                listing_span.in_scope(|| {
                    Self::session_list_response_for_workspace(
                        &args,
                        listings,
                        requested_workspace.as_ref().map(AcpSessionWorkspace::path),
                    )
                })
            })
            .await
            .map_err(|error| {
                log_suppressed_error(&error, "session_list", "archive_worker_failed");
                safe_internal_error("session_list", "archive_worker_failed", "Unable to list archived ACP sessions")
            })??;
            debug!(
                session_count = response.sessions.len(),
                has_more = response.next_cursor.is_some(),
                "Listed durable ACP sessions"
            );
            Ok(response)
        })
        .await
    }

    /// Programmatic equivalent of the SACP `session/load` handler.
    pub(crate) async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, acp::Error> {
        trace_storage_operation("session_load", async {
            let workspace = self.workspace_policy.resolve_client_cwd(&args.cwd).await?;
            let session = self
                .attach_or_get_session(&args.session_id, &workspace)
                .await
                .map_err(|error| {
                    log_suppressed_error(&error, "session_load", "archive_restore_failed");
                    safe_internal_error(
                        "session_load",
                        "archive_restore_failed",
                        "Unable to load the requested ACP session",
                    )
                })?;
            self.ensure_task_lifecycle_forwarder(&session).map_err(|error| {
                log_suppressed_error(&error, "session_load", "task_lifecycle_setup_failed");
                safe_internal_error(
                    "session_load",
                    "task_lifecycle_setup_failed",
                    "Unable to initialize ACP session updates",
                )
            })?;

            if let Err(error) = self.replay_persisted_task_plan(&session, &args.session_id).await {
                log_suppressed_error(&error, "session_load", "task_plan_replay_failed");
                warn!(
                    operation = "session_load",
                    error_category = "task_plan_replay_failed",
                    "Failed to replay persisted ACP task plan"
                );
            }

            if let Err(error) = self.send_available_commands_update(&args.session_id).await {
                log_suppressed_error(&error, "session_load", "command_update_failed");
                warn!(
                    operation = "session_load",
                    error_category = "command_update_failed",
                    "Failed to advertise ACP slash commands"
                );
            }

            let config_options = self.current_session_config_options(&session).map_err(|error| {
                log_suppressed_error(&error, "session_load", "session_configuration_failed");
                safe_internal_error(
                    "session_load",
                    "session_configuration_failed",
                    "Unable to read ACP session configuration",
                )
            })?;
            self.run_session_start_hooks(&session).await.map_err(|error| {
                log_suppressed_error(&error, "session_load", "session_start_hook_failed");
                safe_internal_error(
                    "session_load",
                    "session_start_hook_failed",
                    "Unable to run ACP session start hooks",
                )
            })?;
            debug!("Loaded durable ACP session through legacy session/load");
            Ok(acp::LoadSessionResponse::new().config_options(config_options))
        })
        .await
    }

    /// Programmatic equivalent of the SACP `session/resume` handler.
    pub(crate) async fn resume_session(
        &self,
        args: acp::ResumeSessionRequest,
    ) -> Result<acp::ResumeSessionResponse, acp::Error> {
        trace_storage_operation("session_resume", async {
            let workspace = self.workspace_policy.resolve_client_cwd(&args.cwd).await?;
            let session = self
                .attach_or_get_session(&args.session_id, &workspace)
                .await
                .map_err(|error| {
                    log_suppressed_error(&error, "session_resume", "archive_restore_failed");
                    safe_internal_error(
                        "session_resume",
                        "archive_restore_failed",
                        "Unable to resume the requested ACP session",
                    )
                })?;
            self.ensure_task_lifecycle_forwarder(&session).map_err(|error| {
                log_suppressed_error(&error, "session_resume", "task_lifecycle_setup_failed");
                safe_internal_error(
                    "session_resume",
                    "task_lifecycle_setup_failed",
                    "Unable to initialize ACP session updates",
                )
            })?;

            if let Err(error) = self.replay_persisted_task_plan(&session, &args.session_id).await {
                log_suppressed_error(&error, "session_resume", "task_plan_replay_failed");
                warn!(
                    operation = "session_resume",
                    error_category = "task_plan_replay_failed",
                    "Failed to replay persisted ACP task plan"
                );
            }

            if let Err(error) = self.send_available_commands_update(&args.session_id).await {
                log_suppressed_error(&error, "session_resume", "command_update_failed");
                warn!(
                    operation = "session_resume",
                    error_category = "command_update_failed",
                    "Failed to advertise ACP slash commands"
                );
            }

            let config_options = self.current_session_config_options(&session).map_err(|error| {
                log_suppressed_error(&error, "session_resume", "session_configuration_failed");
                safe_internal_error(
                    "session_resume",
                    "session_configuration_failed",
                    "Unable to read ACP session configuration",
                )
            })?;
            self.run_session_start_hooks(&session).await.map_err(|_error| {
                safe_internal_error(
                    "session_resume",
                    "session_start_hook_failed",
                    "Unable to run ACP session start hooks",
                )
            })?;
            debug!("Resumed durable ACP session");
            Ok(acp::ResumeSessionResponse::new().config_options(config_options))
        })
        .await
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WorkspaceMatch {
    Matches,
    Differs,
}

fn canonical_workspace_match(stored_workspace: &Path, requested_workspace: &Path) -> anyhow::Result<WorkspaceMatch> {
    let canonical_stored_workspace =
        diapason_commons::canonicalize(stored_workspace).context("resolving stored ACP session workspace")?;
    Ok(if canonical_stored_workspace == requested_workspace {
        WorkspaceMatch::Matches
    } else {
        WorkspaceMatch::Differs
    })
}

/// Compares an archived listing's workspace with the requested one.
///
/// Fails when the listing carries no stored workspace path, because resuming
/// in an unknown directory would defeat the workspace isolation check.
pub(super) fn session_listing_workspace_match(
    listing: &SessionListing,
    requested_workspace: &Path,
) -> anyhow::Result<WorkspaceMatch> {
    let stored_workspace = session_workspace_path(listing)
        .ok_or_else(|| anyhow::anyhow!("archived session '{}' has no stored workspace path", listing.identifier()))?;
    canonical_workspace_match(&stored_workspace, requested_workspace)
}

/// Repairs unresolved archived calls into uncertain-effect placeholders for checkpointing.
pub(super) fn repair_archived_tool_calls(
    listing: &mut SessionListing,
) -> Result<RecoveryReport, crate::zed::agent::tool_recovery::AmbiguousToolResultHistory> {
    let mut messages = messages_from_session_listing(listing);
    let report = repair_unresolved_tool_calls(&mut messages)?;
    if report.history_changed() {
        listing.snapshot.total_messages = messages.len();
        listing.snapshot.messages = messages.iter().map(SessionMessage::from).collect();
    }
    Ok(report)
}
