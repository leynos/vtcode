//! Durable archive persistence and workspace-bound session restoration.
use anyhow::Context;
use futures::future::BoxFuture;
use std::future::Future;
use std::path::Path;
use std::time::Instant;
use tracing::{Instrument, debug, warn};
use vtcode_core::core::threads::ThreadBootstrap;
use vtcode_core::hooks::SessionStartTrigger;
use vtcode_core::llm::provider::MessageRole;
use vtcode_core::utils::session_archive::{
    SessionArchive, SessionListing, SessionMessage, SessionProgressArgs, SessionProgressPersistenceStatus,
    find_session_by_identifier, history_persistence_enabled, list_recent_sessions, session_workspace_path,
};

use crate::acp;
use crate::zed::session::AcpSessionWorkspace;
use crate::zed::types::{SessionHandle, lock_poison_error};

use super::{SessionHandleBootstrap, ZedAgent, safe_internal_error, safe_invalid_params_error};
use crate::zed::agent::log_suppressed_error;

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
    pub(in crate::zed::agent) async fn checkpoint_session(&self, session: &SessionHandle) -> anyhow::Result<()> {
        trace_storage_operation("checkpoint", self.checkpoint_session_inner(session)).await
    }

    async fn checkpoint_session_inner(&self, session: &SessionHandle) -> anyhow::Result<()> {
        if !history_persistence_enabled() {
            debug!("Skipped ACP session checkpoint because history persistence is disabled");
            return Ok(());
        }

        let (existing_archive, snapshot, session_identifier) = {
            let data = session
                .data
                .lock()
                .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
            let snapshot = data.thread.snapshot();
            (data.archive.clone(), snapshot, data.session_id.0.to_string())
        };

        let metadata = snapshot
            .metadata
            .clone()
            .ok_or_else(|| anyhow::anyhow!("ACP session is missing archive metadata"))?;
        let mut archive = match existing_archive {
            Some(archive) => archive,
            None => {
                let archive = SessionArchive::new_with_identifier(metadata.clone(), session_identifier).await?;
                let mut data = session
                    .data
                    .lock()
                    .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
                data.archive = Some(archive.clone());
                archive
            }
        };
        archive.replace_metadata(metadata);

        let messages = snapshot.messages.iter().map(SessionMessage::from).collect::<Vec<_>>();
        let recent_start = messages.len().saturating_sub(20);
        let recent_messages = messages.iter().skip(recent_start).cloned().collect();
        let turn_number = snapshot
            .messages
            .iter()
            .filter(|message| message.role == MessageRole::User)
            .count();
        let persistence_status = archive
            .persist_checkpoint_async(SessionProgressArgs {
                total_messages: messages.len(),
                distinct_tools: Vec::new(),
                messages,
                recent_messages,
                turn_number,
                token_usage: None,
                max_context_tokens: None,
                loaded_skills: Some(snapshot.loaded_skills),
                turn_diagnostics: None,
            })
            .await?;
        let persistence_outcome = match persistence_status {
            SessionProgressPersistenceStatus::Persisted(_) => "persisted",
            SessionProgressPersistenceStatus::Throttled(_) => "throttled",
            SessionProgressPersistenceStatus::Disabled(_) => "disabled",
        };
        debug!(persistence_outcome, "Processed ACP session checkpoint");
        Ok(())
    }

    async fn attach_thread_from_archive(
        &self,
        session_id: &acp::SessionId,
        identifier: &str,
        workspace: &AcpSessionWorkspace,
    ) -> anyhow::Result<SessionHandle> {
        trace_storage_operation("archive_attach", async {
            let listing = find_session_by_identifier(identifier)
                .await?
                .ok_or_else(|| anyhow::anyhow!("unknown archived session '{identifier}'"))?;
            let workspace_match = session_listing_workspace_match(&listing, workspace.path())
                .with_context(|| format!("resolving workspace ownership for archived session '{identifier}'"))?;
            anyhow::ensure!(
                workspace_match == WorkspaceMatch::Matches,
                "archived session '{identifier}' belongs to a different workspace"
            );
            let archive = SessionArchive::resume_from_listing(&listing, listing.snapshot.metadata.clone());
            let thread = self
                .thread_manager
                .start_thread_with_identifier(listing.identifier(), ThreadBootstrap::from_listing(listing));
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
            let insert_result = self
                .sessions
                .lock()
                .map_err(|error| lock_poison_error("registering resumed ACP session", error))
                .map(|mut sessions| drop(sessions.insert(session_id.clone(), handle.clone())));
            if let Err(error) = insert_result {
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
            Ok(handle)
        })
        .await
    }

    pub(super) async fn attach_or_get_session(
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
                .map(vtcode_commons::canonicalize)
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
        vtcode_commons::canonicalize(stored_workspace).context("resolving stored ACP session workspace")?;
    Ok(if canonical_stored_workspace == requested_workspace {
        WorkspaceMatch::Matches
    } else {
        WorkspaceMatch::Differs
    })
}

pub(super) fn session_listing_workspace_match(
    listing: &SessionListing,
    requested_workspace: &Path,
) -> anyhow::Result<WorkspaceMatch> {
    let stored_workspace = session_workspace_path(listing)
        .ok_or_else(|| anyhow::anyhow!("archived session '{}' has no stored workspace path", listing.identifier()))?;
    canonical_workspace_match(&stored_workspace, requested_workspace)
}
