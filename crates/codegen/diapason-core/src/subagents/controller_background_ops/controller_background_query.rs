//! Read-only projections over background subprocess state, threads and output.

use self::controller_background_liveness::{bounded_liveness_reason, exec_session_is_missing};

use super::controller_background_lifecycle::background_launch_is_owed;
use super::controller_background_settlement::restart_allowed;

use super::*;

/// Why a record this controller does not drive cannot be observed.
///
/// Another session's launch runs in that session's process, so this
/// controller's exec manager cannot see it. A missing read is then no evidence
/// of an exit, and the entry keeps its cached status with this bounded reason.
const FOREIGN_BACKGROUND_LIVENESS_REASON: &str =
    "background subprocess belongs to another session, so this controller cannot observe its exec session";

/// One background record captured for the liveness overlay.
///
/// The projection and the facts the overlay decides from are read under one
/// state lock, so a decision cannot mix two versions of the record.
struct BackgroundLivenessCapture {
    entry: BackgroundSubprocessEntry,
    active: bool,
    /// Whether this controller drives — and so can observe — the launch.
    driven: bool,
    exec_session_id: String,
    observed_status: BackgroundSubprocessStatus,
    desired_enabled: bool,
    restart_attempts: u8,
    liveness: BackgroundLiveness,
}

/// Resolves a caller-supplied background record id to the loaded map key.
///
/// The map is keyed by canonical owner-scoped id, while a client can still hold
/// the workspace-global id an older release persisted. An exact lookup then
/// misses a running record, so the pre-owner-scoping id is tried next. The
/// caller's authority decides which candidate that fallback may select:
///
/// - An exact key must be addressable by `owner`; a foreign exact match is not
///   the caller's record, so the alias search runs instead.
/// - An alias candidate is considered only when `owner` may address it. This is
///   the precedence rule: a session's own record always beats another's, and
///   an ownerless legacy row can neither shadow nor precede it. Candidates are
///   examined in map key order.
/// - [`TaskOwner::Workspace`] may address every record, so the first alias
///   match in key order wins, preserving the permissive operator view.
/// - [`TaskOwner::SessionTree`] proves nothing about a background record (see
///   [`TaskOwner::background_owned`]), so it resolves nothing here.
///
/// The ownership rejection each call site applies after resolution stays in
/// place. Narrowing the fallback keeps a foreign or ownerless row from
/// shadowing the caller's own record; it does not replace that check.
fn resolve_background_key(state: &ControllerState, owner: TaskOwner<'_>, target: &str) -> Option<String> {
    if state
        .background_children
        .get(target)
        .is_some_and(|record| owner.background_owned(record.owner_session_id.as_deref()))
    {
        return Some(target.to_owned());
    }
    state
        .background_children
        .iter()
        .find(|(_, record)| {
            background_record_id(record.agent_name.as_str()) == target
                && owner.background_owned(record.owner_session_id.as_deref())
        })
        .map(|(key, _)| key.clone())
}

/// Resolves a caller-supplied id to the record it names for `owner`.
///
/// Only a record `owner` may address is selected; see
/// [`resolve_background_key`] for the precedence rule.
pub(super) fn resolve_background_target<'a>(
    state: &'a ControllerState,
    owner: TaskOwner<'_>,
    target: &str,
) -> Option<&'a BackgroundRecord> {
    state.background_children.get(&resolve_background_key(state, owner, target)?)
}

/// Resolves a caller-supplied id to the record it names for `owner`, for mutation.
pub(super) fn resolve_background_target_mut<'a>(
    state: &'a mut ControllerState,
    owner: TaskOwner<'_>,
    target: &str,
) -> Option<&'a mut BackgroundRecord> {
    state
        .background_children
        .get_mut(&resolve_background_key(state, owner, target)?)
}

/// Whether a non-live liveness observation still describes the record it was
/// taken for.
///
/// The query captures the record, releases the state lock for the exec-session
/// read, then projects an exit from the captured flags. A cancellation, a
/// restart or a replacement can land in that window. The launch id, the
/// status, the desired flag and the restart budget are the facts the
/// projection consults, so a change to any of them makes the observation
/// stale; the caller then keeps the record's current projection instead of
/// relabelling it from outdated flags. The status is part of that set because
/// a spawn committing `Running` changes what a captured `Starting` would mean:
/// projecting the captured phase would report a launch that has since started
/// as still starting, or settle it from an absence observed before the commit.
pub(super) fn liveness_observation_is_current(
    record: &BackgroundRecord,
    exec_session_id: &str,
    status: BackgroundSubprocessStatus,
    desired_enabled: bool,
    restart_attempts: u8,
) -> bool {
    record.exec_session_id == exec_session_id
        && record.status == status
        && record.desired_enabled == desired_enabled
        && record.restart_attempts == restart_attempts
}

impl SubagentController {
    /// Returns status entries for the tracked background subprocesses `owner`
    /// may address.
    ///
    /// `owner` is the authority the caller presents, exactly as the other
    /// background queries require: a session sees only the records it owns, and
    /// the workspace operator sees every record the controller holds. A
    /// session-tree set proves nothing about a background record (see
    /// [`TaskOwner::background_owned`]), so it selects none.
    pub async fn background_status_entries(&self, owner: TaskOwner<'_>) -> Vec<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        state
            .background_children
            .values()
            .filter(|record| owner.background_owned(record.owner_session_id.as_deref()))
            .map(BackgroundRecord::build_status_entry)
            .collect()
    }

    /// Returns the cached status projection with each active record's
    /// liveness overlaid from the exec-session manager.
    ///
    /// This is a pure query: unlike
    /// [`Self::refresh_background_processes`] it mutates no record, never
    /// respawns, never publishes a lifecycle update and never persists. A
    /// management list uses it so a read cannot restart work, while still
    /// reporting a process whose exec session has already ended.
    ///
    /// The read distinguishes a session that is genuinely absent from a read
    /// that failed for another reason. An absent session is the expected exit
    /// and is projected exactly as before. Any other fault keeps the record's
    /// cached status and marks the entry [`BackgroundLiveness::Unobserved`], so
    /// a listing never fabricates a terminal outcome it did not observe. One
    /// unobserved record does not abort the list.
    ///
    /// The record is captured under one read lock and the exec session is read
    /// without it, so a cancellation or replacement can land in between. Before
    /// projecting a non-live observation the record is re-read: when its launch,
    /// its flags or its restart budget changed, the entry keeps the record's
    /// current projection rather than the captured outcome, which would
    /// otherwise report a task that a cancellation has since settled as a
    /// failure. The launch obligation is read under that same lock, so a commit
    /// that stores `Running` and clears the obligation cannot be observed as a
    /// claim with nothing left to drive it.
    ///
    /// `owner` is the authority the caller presents: only the records it may
    /// address are projected, under the same rule as
    /// [`Self::background_status_entries`].
    pub async fn background_status_entries_with_liveness(&self, owner: TaskOwner<'_>) -> Vec<BackgroundLivenessEntry> {
        let mut captures = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| owner.background_owned(record.owner_session_id.as_deref()))
                .map(|record| BackgroundLivenessCapture {
                    entry: record.build_status_entry(),
                    active: record.status.is_active(),
                    driven: self.background_record_restart_is_authorized(record),
                    exec_session_id: record.exec_session_id.clone(),
                    observed_status: record.status,
                    desired_enabled: record.desired_enabled,
                    restart_attempts: record.restart_attempts,
                    liveness: BackgroundLiveness::Observed,
                })
                .collect::<Vec<_>>()
        };

        for capture in &mut captures {
            if !capture.active {
                // A terminal task carries an end instant on the wire: the ACP
                // contract allows null only while a task is active, and a
                // terminal record restored without one would otherwise send
                // null. The fallback is the record's own last observed instant,
                // which is what the active paths below already use, so repeated
                // polls of one unchanged record agree. Active records keep null.
                capture.entry.ended_at.get_or_insert(capture.entry.updated_at);
                continue;
            }
            // Another session's launch runs in that session's process. Its exec
            // session is invisible here, so re-reading the cached record cannot
            // distinguish a stopped process from one this controller simply
            // cannot see; keep the cached status and report it unobserved
            // rather than inventing a terminal outcome.
            if !capture.driven {
                capture.liveness = BackgroundLiveness::Unobserved {
                    reason: FOREIGN_BACKGROUND_LIVENESS_REASON.to_string(),
                };
                continue;
            }
            let alive = if capture.exec_session_id.is_empty() {
                false
            } else {
                match self.config.exec_sessions.snapshot_session(&capture.exec_session_id).await {
                    Ok(snapshot) => matches!(
                        snapshot.lifecycle_state,
                        Some(crate::tools::types::DiapasonSessionLifecycleState::Running)
                    ),
                    Err(error) if exec_session_is_missing(&error) => false,
                    Err(error) => {
                        // Nothing is known about the process, so keep the
                        // cached status instead of projecting an exit.
                        capture.liveness = BackgroundLiveness::Unobserved {
                            reason: bounded_liveness_reason(&error, &capture.exec_session_id),
                        };
                        continue;
                    }
                }
            };
            if alive {
                continue;
            }
            // The observation was taken outside the state lock, so the record
            // it describes may have moved since it was captured. Re-read it by
            // id and keep the record's own projection when the launch, its
            // flags or its restart budget changed: projecting the captured
            // flags would report a cancellation that already settled `Stopped`
            // as a failure, or settle a launch another caller has replaced.
            let record_id = capture.entry.id.clone();
            let exec_session_id = capture.exec_session_id.clone();
            // The obligation is read under the same read lock that revalidates
            // the record. A commit stores `Running` and clears the launch
            // obligation under this lock's write side, so reading the two apart
            // could combine the earlier `Starting` status with a cleared
            // obligation and list a launch that just started as failed. This is
            // also the settlement path's lock order: the state, then the
            // pending-launch registry.
            let current = {
                let state = self.state.read().await;
                state.background_children.get(&record_id).map(|record| {
                    (
                        record.build_status_entry(),
                        liveness_observation_is_current(
                            record,
                            &exec_session_id,
                            capture.observed_status,
                            capture.desired_enabled,
                            capture.restart_attempts,
                        ),
                        background_launch_is_owed(&self.state, &record_id, &exec_session_id),
                    )
                })
            };
            // A cached `Starting` status is only meaningful while a launch is
            // still owed for it. A record whose claim was abandoned before its
            // process started keeps `Starting` with nothing left to drive it:
            // its driver dropped the obligation, so refresh will settle it on
            // the next pass. Reporting that claim as an active task here would
            // list a launch that is not running and never will, which is the
            // opposite of what a liveness projection is for. Such a record is
            // left to the refresh path, and this read-only query projects the
            // failure the settlement will write.
            let launch_owed = match current {
                Some((mut current_entry, false, _)) => {
                    // The record's own outcome is authoritative, including the
                    // wire contract's terminal instant.
                    if !current_entry.status.is_active() {
                        current_entry.ended_at.get_or_insert(current_entry.updated_at);
                    }
                    capture.entry = current_entry;
                    continue;
                }
                // The record no longer exists, so there is no current outcome to
                // project. Keeping the captured entry invents nothing.
                None => continue,
                Some((_, true, launch_owed)) => launch_owed,
            };
            // An orphaned `Starting` claim is handled apart from a missing
            // `Running` record. A claim nothing owes a launch for has no driver:
            // its reservation was abandoned, and only a later refresh can
            // schedule a replacement. This read starts nothing, so it must not
            // report the claim as an active task merely because the restart
            // budget would allow one. A missing `Running` record is different:
            // its process is observed gone, and `restart_allowed` then predicts
            // the replacement refresh will schedule.
            let orphaned_starting_claim = capture.entry.status == BackgroundSubprocessStatus::Starting && !launch_owed;
            capture.entry.status = if orphaned_starting_claim {
                if capture.desired_enabled {
                    BackgroundSubprocessStatus::Error
                } else {
                    BackgroundSubprocessStatus::Stopped
                }
            } else if capture.desired_enabled
                && ((capture.entry.status == BackgroundSubprocessStatus::Starting && launch_owed)
                    || restart_allowed(self.config.vt_cfg.subagents.background.auto_restore, capture.restart_attempts))
            {
                BackgroundSubprocessStatus::Starting
            } else if capture.desired_enabled {
                BackgroundSubprocessStatus::Error
            } else {
                BackgroundSubprocessStatus::Stopped
            };
            // The ACP wire contract requires a number for a terminal task's
            // end timestamp and allows null only while the task is active. A
            // record that never observed an end instant therefore gets its own
            // last observed instant, not the query time, so repeated polls of
            // one unchanged record report the same end.
            if !capture.entry.status.is_active() {
                capture.entry.ended_at.get_or_insert(capture.entry.updated_at);
            }
        }

        captures
            .into_iter()
            .map(|capture| BackgroundLivenessEntry { entry: capture.entry, liveness: capture.liveness })
            .collect()
    }

    /// Returns a cached snapshot of a background subprocess including its
    /// preview output.
    ///
    /// This is a pure query over cached state: it never refreshes, so it never
    /// respawns a process, settles a record, publishes a lifecycle update or
    /// persists the ledger. Callers that need exit detection must call
    /// [`Self::refresh_then_background_snapshot`] or
    /// [`Self::refresh_background_processes`] explicitly.
    pub async fn background_snapshot(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessSnapshot> {
        let entry = {
            let state = self.state.read().await;
            let record = resolve_background_target(&state, owner, target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
            if !owner.background_owned(record.owner_session_id.as_deref()) {
                bail!("Unknown background subprocess {target}");
            }
            record.build_status_entry()
        };

        let preview = read_preview(self.output_reader.as_ref(), &entry, target).await?;

        Ok(BackgroundSubprocessSnapshot { entry, preview })
    }

    /// Returns a cached status entry without reading its preview.
    ///
    /// The caller supplies the authority it presents, exactly as the other
    /// background commands do, so an id is resolved by the same owner-aware
    /// rule: a legacy `background-<agent>` alias opens the record it names
    /// rather than only an id that happens to match a map key.
    pub async fn background_status_entry(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let record = resolve_background_target(&state, owner, target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        if !owner.background_owned(record.owner_session_id.as_deref()) {
            bail!("Unknown background subprocess {target}");
        }
        Ok(record.build_status_entry())
    }

    /// Refreshes background state, then returns the snapshot.
    ///
    /// The name states the side effect: [`Self::refresh_background_processes`]
    /// may settle a record, publish a lifecycle update, persist the ledger and
    /// respawn a process whose restart budget allows it. Use this when the
    /// caller needs the current exit state; use [`Self::background_snapshot`]
    /// when a cached read is enough.
    pub async fn refresh_then_background_snapshot(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessSnapshot> {
        let _ = self.refresh_background_processes().await?;
        self.background_snapshot(owner, target).await
    }

    /// Returns a caller-selected tail of background output.
    ///
    /// This is a read-only query: it reads the cached record and the live exec
    /// session output without refreshing process state, publishing lifecycle
    /// updates, or persisting state. Callers that need exit detection run
    /// [`Self::refresh_background_processes`] first as an explicit command.
    ///
    /// `max_lines` is the caller's already-validated line bound. Transport
    /// limits, defaults, and request validation belong to the adapter that
    /// owns the transport contract; this controller only applies the bound it
    /// is given. The existing [`Self::background_snapshot`] 24-line preview is
    /// unchanged.
    ///
    /// Storage access and archive fallback are delegated to the controller's
    /// [`BackgroundOutputReader`]; the controller keeps the line bound and the
    /// error mapping.
    pub async fn background_output_tail(&self, owner: TaskOwner<'_>, target: &str, max_lines: usize) -> Result<String> {
        let entry = {
            let state = self.state.read().await;
            let record = resolve_background_target(&state, owner, target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
            if !owner.background_owned(record.owner_session_id.as_deref()) {
                bail!("Unknown background subprocess {target}");
            }
            record.build_status_entry()
        };

        read_output_tail(self.output_reader.as_ref(), &entry, target, max_lines).await
    }

    /// Returns whether background subagents are enabled in the configuration.
    #[must_use]
    pub fn background_subagents_enabled(&self) -> bool {
        self.config.vt_cfg.subagents.background.enabled
    }

    /// Returns the configured default background agent name, if any.
    #[must_use]
    pub fn configured_default_background_agent(&self) -> Option<&str> {
        self.config
            .vt_cfg
            .subagents
            .background
            .default_agent
            .as_deref()
            .map(str::trim)
            .filter(|agent| !agent.is_empty())
    }
}

/// Foreground-only thread snapshot projection for one tracked child.
///
/// The query surface above reads background state and output; a foreground
/// child snapshot instead joins its captured runtime configuration, thread
/// handle, session archive and stored messages. Keeping that projection in a
/// private child module leaves the public method and every other query
/// unchanged while the background module stays focused.
mod thread_snapshot {
    use super::*;

    /// The child state one snapshot projection reads under a single lock.
    struct CapturedSubagentThread {
        id: String,
        session_id: String,
        parent_thread_id: String,
        agent_name: String,
        display_label: String,
        status: SubagentStatus,
        background: bool,
        created_at: chrono::DateTime<Utc>,
        updated_at: chrono::DateTime<Utc>,
        archive_path: Option<PathBuf>,
        transcript_path: Option<PathBuf>,
        effective_config: DiapasonConfig,
        thread_handle: Option<ThreadRuntimeHandle>,
        archive_metadata: Option<crate::utils::session_archive::SessionArchiveMetadata>,
        stored_messages: Vec<Message>,
        recent_events: Vec<crate::exec::events::ThreadEvent>,
    }

    impl SubagentController {
        /// Returns a thread snapshot for a tracked child subagent by its target id.
        pub async fn snapshot_for_thread(&self, owner: TaskOwner<'_>, target: &str) -> Result<SubagentThreadSnapshot> {
            self.ensure_child_owned(owner, target).await?;
            let captured = self.capture_subagent_thread(target).await?;
            let snapshot = self.materialize_thread_snapshot(&captured).await;
            Ok(SubagentThreadSnapshot {
                id: captured.id,
                session_id: captured.session_id,
                parent_thread_id: captured.parent_thread_id,
                agent_name: captured.agent_name,
                display_label: captured.display_label,
                status: captured.status,
                background: captured.background,
                created_at: captured.created_at,
                updated_at: captured.updated_at,
                archive_path: captured.archive_path,
                transcript_path: captured.transcript_path,
                effective_config: captured.effective_config,
                snapshot,
                recent_events: captured.recent_events,
            })
        }

        /// Reads the child record and the runtime state one snapshot needs.
        async fn capture_subagent_thread(&self, target: &str) -> Result<CapturedSubagentThread> {
            let state = self.state.read().await;
            let record = state
                .children
                .get(target)
                .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
            let effective_config = record
                .effective_config
                .clone()
                .ok_or_else(|| anyhow!("Subagent {target} does not have a captured runtime configuration yet"))?;
            Ok(CapturedSubagentThread {
                id: record.id.clone(),
                session_id: record.session_id.clone(),
                parent_thread_id: record.parent_thread_id.clone(),
                agent_name: record.spec.name.clone(),
                display_label: record.display_label.clone(),
                status: record.status,
                background: record.background,
                created_at: record.created_at,
                updated_at: record.updated_at,
                archive_path: record.archive_path.clone(),
                transcript_path: record.transcript_path.clone(),
                effective_config,
                thread_handle: record.thread_handle.clone(),
                archive_metadata: record.archive_metadata.clone(),
                stored_messages: record.stored_messages.clone(),
                recent_events: record
                    .thread_handle
                    .as_ref()
                    .map(ThreadRuntimeHandle::recent_events)
                    .unwrap_or_default(),
            })
        }

        /// Builds the snapshot from the live handle, the archive, or the
        /// captured configuration, in that order.
        async fn materialize_thread_snapshot(&self, captured: &CapturedSubagentThread) -> ThreadSnapshot {
            if let Some(handle) = captured.thread_handle.as_ref() {
                return handle.snapshot();
            }

            let archive_listing = match captured.archive_path.as_ref() {
                Some(path) if tokio::fs::metadata(path).await.is_ok() => load_session_listing(path).await.ok(),
                _ => None,
            };
            let metadata = archive_listing
                .as_ref()
                .map(|listing| listing.snapshot.metadata.clone())
                .or_else(|| captured.archive_metadata.clone())
                .or_else(|| {
                    Some(crate::core::threads::build_thread_archive_metadata(
                        &self.config.workspace_root,
                        captured.effective_config.agent.default_model.as_str(),
                        captured.effective_config.agent.provider.as_str(),
                        captured.effective_config.agent.theme.as_str(),
                        captured.effective_config.agent.reasoning_effort.as_str(),
                    ))
                });
            ThreadSnapshot {
                thread_id: ThreadId::new(captured.session_id.clone()),
                metadata,
                archive_listing,
                messages: captured.stored_messages.clone(),
                loaded_skills: Vec::new(),
                turn_in_flight: false,
            }
        }
    }
}

/// Reads the bounded preview through the task-keyed reader.
///
/// The controller builds a [`BackgroundTaskRef`] from the live record it holds,
/// so the reader sees the task identity and its own storage handles rather than
/// the whole record; no exec-session or path handle crosses this boundary into
/// the caller.
///
/// A task without output yet yields an empty preview, which is the absence of
/// output rather than a fault. Every read fault is propagated with the caller's
/// target in the context, for the live session and for the transcript or
/// archive alike: the archive read is I/O, so reporting its failure as an empty
/// preview would hide a real storage failure behind the same value used for "not
/// written yet". Callers that cannot act on the fault, such as a background
/// sidebar, handle the error at their own boundary instead.
pub(super) async fn read_preview(
    reader: &dyn BackgroundOutputReader,
    entry: &BackgroundSubprocessEntry,
    target: &str,
) -> Result<String> {
    let task = BackgroundTaskRef::from_entry(entry);
    match reader.read_output(&task).await {
        Ok(BackgroundOutput::Live(output) | BackgroundOutput::Archived(output)) => {
            Ok(extract_tail_lines(&output, SUBAGENT_PREVIEW_LINES))
        }
        Ok(BackgroundOutput::NotReady) => Ok(String::new()),
        Err(error) => Err(anyhow::Error::new(error)).with_context(|| format!("read background output for {target}")),
    }
}

/// Reads the bounded output tail through the task-keyed reader.
///
/// The controller builds a [`BackgroundTaskRef`] from the live record it holds,
/// so the reader sees the task identity and its own storage handles rather than
/// the whole record; no exec-session or path handle crosses this boundary into
/// the caller. The reader owns storage access and fallback selection, while the
/// controller keeps the line bound. Any read fault is propagated with the
/// caller's target in the context.
pub(super) async fn read_output_tail(
    reader: &dyn BackgroundOutputReader,
    entry: &BackgroundSubprocessEntry,
    target: &str,
    max_lines: usize,
) -> Result<String> {
    let task = BackgroundTaskRef::from_entry(entry);
    match reader.read_output(&task).await {
        Ok(BackgroundOutput::Live(output) | BackgroundOutput::Archived(output)) => {
            Ok(extract_tail_lines(&output, max_lines))
        }
        Ok(BackgroundOutput::NotReady) => Ok(String::new()),
        Err(error) => Err(anyhow::Error::new(error)).with_context(|| format!("read background output for {target}")),
    }
}

#[cfg(test)]
mod foreign_liveness_tests {
    //! Pins that a record this controller does not drive is never projected as
    //! an exit it cannot have observed.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::*;

    const WORKER: &str = "background-worker";
    const WORKER_AGENT: &str = "worker";
    const FOREIGN_EXEC: &str = "exec-foreign";

    fn test_config(workspace_root: PathBuf) -> SubagentControllerConfig {
        let vt_cfg = DiapasonConfig::default();
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
        let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        SubagentControllerConfig {
            workspace_root,
            parent_session_id: "parent-session".to_string(),
            parent_model: "gpt-5.6-sol".to_string(),
            parent_provider: "openai".to_string(),
            parent_reasoning_effort: ReasoningEffortLevel::Medium,
            api_key: "test-key".to_string(),
            vt_cfg,
            openai_chatgpt_auth: None,
            depth: 0,
            workspace_gated: false,
            exec_sessions,
            output_reader: None,
            pty_manager: pty_sessions.manager().clone(),
            managed_background_runtime: false,
            lock_scope: None,
        }
    }

    /// A running record owned by another session.
    fn foreign_record() -> BackgroundRecord {
        BackgroundRecord {
            id: WORKER.to_string(),
            owner_session_id: Some("other-session".to_string()),
            agent_name: WORKER_AGENT.to_string(),
            display_label: WORKER_AGENT.to_string(),
            description: WORKER_AGENT.to_string(),
            source: "builtin".to_string(),
            colour: None,
            session_id: format!("session-{WORKER}"),
            parent_session_id: "other-session".to_string(),
            exec_session_id: FOREIGN_EXEC.to_string(),
            desired_enabled: true,
            status: BackgroundSubprocessStatus::Running,
            created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
            updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
            started_at: None,
            ended_at: None,
            pid: None,
            prompt: "work".to_string(),
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
        }
    }

    /// A foreign launch this controller's exec manager cannot see must not be
    /// reported as exited.
    ///
    /// The workspace listing includes records owned by other sessions, whose
    /// processes run in that session's process. A typed missing result here
    /// says nothing about the process, so with auto-restore off the old
    /// projection turned a live task into `Error`; the entry must instead keep
    /// its cached status and report that liveness was unobserved.
    #[tokio::test]
    async fn a_foreign_record_is_unobserved_rather_than_exited() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), foreign_record());
        }

        let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
        let entry = entries
            .iter()
            .find(|entry| entry.entry.id == WORKER)
            .expect("the workspace listing includes a record owned by another session");

        assert_eq!(
            entry.entry.status,
            BackgroundSubprocessStatus::Running,
            "a foreign record keeps its cached status instead of being projected as an exit"
        );
        match &entry.liveness {
            BackgroundLiveness::Unobserved { reason } => {
                assert!(reason.contains("another session"), "the reason names why it is unobserved: {reason}");
            }
            BackgroundLiveness::Observed => panic!("a foreign record's liveness cannot be observed here"),
        }

        controller.signal_shutdown().await;
    }
}
