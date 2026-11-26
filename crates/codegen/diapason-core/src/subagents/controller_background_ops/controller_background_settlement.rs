//! Settles a background record from a launch or a liveness observation.

use super::*;

use super::controller_background_cancel::background_stop_close_is_pending;
use super::controller_background_lifecycle::{
    background_launch_is_owed, clear_background_launch_owed, mark_background_launch_owed,
};
use super::controller_background_liveness::launch_failure_category;

/// Reports whether a background record may still be given another attempt.
///
/// The single-attempt allowance is domain policy shared by restart settlement
/// and by the retry of a stop whose exec session could not be closed. Both
/// spend the same `restart_attempts` counter, so a record that already spent
/// its attempt cannot keep retrying under a second, unrelated budget.
pub(super) const fn retry_attempt_available(restart_attempts: u8) -> bool {
    restart_attempts < 1
}

/// Reports whether a background record may still be given a replacement run.
///
/// The single restart allowance is domain policy shared by two units: the
/// read-only liveness projection predicts a restart from it, and the settlement
/// path spends it. Both must answer identically, because a list that predicted
/// a restart the settlement path refuses — or the reverse — would report a task
/// as restarting that has really finished, or as finished while a replacement
/// is on its way. Keeping the rule in one function makes the budget a single
/// edit. Automatic restore is an additional condition on top of the shared
/// attempt allowance: a stop close has no such switch, because a stop the
/// caller asked for must always be finished.
pub(super) const fn restart_allowed(auto_restore: bool, restart_attempts: u8) -> bool {
    auto_restore && retry_attempt_available(restart_attempts)
}

/// Reports whether a failed launch still owns the record it wrote.
///
/// A background record id is stable across restarts and shared with the
/// workspace ledger, so the record a launch wrote at `Starting` may no longer
/// describe that launch. A cancellation settles it with `desired_enabled`
/// cleared, and a replacement launch takes the id with a new exec session. An
/// older launch that fails afterwards must not settle either: marking a live
/// replacement as `Error` also makes a later cancellation take the
/// already-terminal branch and skip process termination. The exec session
/// identifies the launch, so only the matching, still-desired, still-starting
/// record may be settled.
pub(super) fn failed_launch_still_owns_record(record: &BackgroundRecord, plan_exec_session_id: &str) -> bool {
    record.exec_session_id == plan_exec_session_id
        && matches!(record.status, BackgroundSubprocessStatus::Starting)
        && record.desired_enabled
}

/// Reports whether a persisted row still claims a launch of its own.
///
/// A ledger row naming another exec session is only a competing launch while it
/// still wants one. Two settled states are not claims:
///
/// - A stopped row is the empty slot an explicit start fills again.
/// - An `Error` row is a launch that is confirmed not to be running: its
///   process either never started or has been observed to exit. Refusing the
///   start would leave a task that failed permanently unstartable, which is the
///   opposite of what a start request is for.
///
/// A failure is only confirmed, though, when nothing still owes a close for it.
/// A stop whose close faulted leaves an `Error` row whose process may still be
/// live, so the reservation checks [`background_record_close_is_unconfirmed`]
/// against the persisted row before it treats that row as free. That check runs
/// ahead of every exemption below, including the one for a row naming this
/// controller's own launch: a close this process still owes is exactly the case
/// that must keep the row unstartable.
///
/// An active row - `Starting` or `Running` - is still a claim, because a process
/// may own it.
pub(super) fn persisted_background_row_is_desired(record: &PersistedBackgroundRecord) -> bool {
    record.desired_enabled && record.status.is_active()
}

/// Controller trees whose last background-ledger write failed.
///
/// A launch commits its record in memory before it saves the ledger, and the
/// save can fail after the process has started. The caller is then told the
/// launch failed while the record says it is running. Reuse must not report
/// that record as durable: a restart, or another process, would never see it.
/// The flag makes the next refresh retry the write before any caller can
/// observe the record. Entries are keyed by the tree's shared state allocation,
/// so every clone observes one history, and a dropped tree's entry is pruned on
/// the next access.
fn pending_ledger_writes() -> &'static Mutex<Vec<PendingLedgerWrite>> {
    static PENDING: std::sync::OnceLock<Mutex<Vec<PendingLedgerWrite>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// One controller tree that owes the ledger a write.
struct PendingLedgerWrite {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    /// Counts the failed saves this tree has not yet recovered from.
    ///
    /// A retry reads this before it writes and passes the value back when it
    /// clears the flag. A save that fails while the retry is in flight bumps
    /// the generation, so the retry's clear no longer matches and the newer
    /// obligation survives. Without it, the clear would discard a mark that
    /// arrived after the retry began, and the next reuse could report an
    /// unwritten record as durable.
    generation: u64,
}

/// Applies `update` to the registry, pruning dropped trees first.
fn with_pending_ledger_writes<R>(update: impl FnOnce(&mut Vec<PendingLedgerWrite>) -> R) -> R {
    let mut registry = pending_ledger_writes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|entry| entry.state.strong_count() > 0);
    update(&mut registry)
}

/// Marks a controller tree as having a record its latest save has not written.
///
/// Returns the generation of the obligation this call created. The marker is
/// raised before the save is awaited, so a concurrent reuse sees the owed
/// write for the whole window in which the in-memory record and the ledger can
/// disagree. A caller that then saves successfully clears exactly the
/// generation it observed.
pub(super) fn mark_background_ledger_write_pending(state: &Arc<RwLock<ControllerState>>) -> u64 {
    with_pending_ledger_writes(|registry| {
        if let Some(entry) = registry
            .iter_mut()
            .find(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
        {
            entry.generation = entry.generation.saturating_add(1);
            return entry.generation;
        }
        registry.push(PendingLedgerWrite { state: Arc::downgrade(state), generation: 1 });
        1
    })
}

/// Returns the obligation a retry is about to discharge, if any.
pub(super) fn pending_background_ledger_write_generation(state: &Arc<RwLock<ControllerState>>) -> Option<u64> {
    with_pending_ledger_writes(|registry| {
        registry
            .iter()
            .find(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
            .map(|entry| entry.generation)
    })
}

/// Clears the flag after the owed write succeeded.
///
/// Only the obligation the caller observed is cleared. A failed save that
/// arrives while the retry is writing bumps the generation, and the newer
/// obligation must survive this clear.
pub(super) fn clear_background_ledger_write_pending(state: &Arc<RwLock<ControllerState>>, observed_generation: u64) {
    with_pending_ledger_writes(|registry| {
        registry.retain(|entry| {
            !std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)) || entry.generation != observed_generation
        });
    });
}

impl SubagentController {
    /// Retries a launch's failed ledger write, reporting whether one was owed.
    ///
    /// A launch commits its record in memory before it saves the ledger, and
    /// the save can fail after the process has started. The caller is then told
    /// the launch failed while the record says it is running. Refresh is the
    /// first step of reuse and of restore, so the owed write is retried here
    /// before a caller can observe a record the ledger never received.
    pub(super) async fn retry_owed_background_ledger_write(&self) -> Result<bool> {
        let Some(generation) = pending_background_ledger_write_generation(&self.state) else {
            return Ok(false);
        };
        self.save_background_state().await?;
        clear_background_ledger_write_pending(&self.state, generation);
        Ok(true)
    }

    /// Settles a background record whose exec session could not be spawned
    /// once no automatic restart remains.
    ///
    /// A record left at `Starting` is reported as an active process forever:
    /// refresh treats `Starting` as active, and the read-only liveness overlay
    /// preserves it while the launch is still desired. Settling it here makes
    /// the failure visible to a management list and keeps the default toggle
    /// from routing a graceful stop to a process that never started. A launch
    /// that still has its restart allowance is left `Starting`, matching the
    /// overlay's restart budget, and the original spawn error is returned
    /// either way.
    ///
    /// Only the launch that failed may settle the record. A record id outlives
    /// one launch: a cancellation can settle the record while its exec session
    /// is still starting, and a replacement launch can take the same id. An
    /// older launch that fails afterwards must not mark the replacement's live
    /// process as failed, because a later cancellation would then take the
    /// already-terminal branch and skip process termination.
    pub(in crate::subagents) async fn settle_failed_background_run(
        &self,
        plan: &BackgroundRunPlan,
        error: &anyhow::Error,
    ) {
        self.settle_background_launch_error(&plan.record_id, &plan.exec_session_id, error, true)
            .await;
    }

    /// Settles a record whose scheduled replacement could not be prepared or claimed.
    ///
    /// The restart allowance was already spent when refresh scheduled the
    /// replacement, so a preparation failure cannot be retried; the same holds
    /// for a claim the ledger would not keep, which may not be retried either
    /// because the process was never started. Leaving the record `Starting`
    /// would report an active launch that nothing will ever drive, and refresh
    /// exempts a `Starting` record only while it still owes a launch, so the
    /// failure is settled here and its obligation discharged. The launch's exec
    /// session still identifies it, so only a record that still names it is
    /// settled.
    pub(in crate::subagents) async fn settle_failed_background_restart(
        &self,
        record_id: &str,
        exec_session_id: &str,
        error: &anyhow::Error,
    ) {
        self.settle_background_launch_error(record_id, exec_session_id, error, false)
            .await;
    }

    /// Applies the settled failure shared by a spawn and a restart preparation.
    ///
    /// `honour_restart_allowance` keeps a first spawn failure `Starting` so the
    /// caller can retry it, while a restart preparation failure is settled
    /// straight away because its allowance is already spent.
    ///
    /// A settled `Error` raises the owed-write marker under the lock that stores
    /// it, so a save that fails or is interrupted leaves a write the next
    /// refresh retries instead of a failure only this process can see. The
    /// marker is discharged only once the save succeeds; the original launch
    /// error is still what the caller receives.
    ///
    /// `record.error` reaches a client as the task's `stopReason`, so it carries
    /// a bounded failure category rather than the error chain: a spawn or claim
    /// fault's chain names ledger, lock and workspace paths, which are local
    /// diagnostic detail and must not travel to a remote peer. The full chain
    /// still reaches this process's log, where a maintainer can read it.
    async fn settle_background_launch_error(
        &self,
        record_id: &str,
        exec_session_id: &str,
        error: &anyhow::Error,
        honour_restart_allowance: bool,
    ) {
        // This launch is finished. Whether it settles `Error` or leaves a
        // remaining allowance for an inline retry, the claim it reserved no
        // longer owes a launch; a replacement records its own key, so clearing
        // this session's obligation cannot hide one.
        clear_background_launch_owed(&self.state, record_id, exec_session_id);
        let write_generation = {
            let mut state = self.state.write().await;
            let Some(record) = state.background_children.get_mut(record_id) else {
                return;
            };
            if !failed_launch_still_owns_record(record, exec_session_id) {
                return;
            }
            if honour_restart_allowance
                && restart_allowed(self.config.vt_cfg.subagents.background.auto_restore, record.restart_attempts)
            {
                return;
            }
            record.status = BackgroundSubprocessStatus::Error;
            // The record's error becomes a client's `stopReason`, so it carries
            // a fixed category and never the error chain: any chain line can
            // quote the ledger, lock or workspace path, which is local
            // diagnostic detail. The chain is logged below, inside this
            // process.
            record.error = Some(format!(
                "Failed to start background subagent '{}': {}",
                record.agent_name,
                launch_failure_category(error)
            ));
            record.summary = Some("Background subagent failed to start".to_string());
            record.updated_at = Utc::now();
            record.ended_at.get_or_insert(Utc::now());
            // The terminal record becomes visible when this lock is released,
            // and reuse could then report a failure the ledger has not
            // received. Register the write under the same lock, so a save that
            // fails or is interrupted leaves the obligation for the next
            // refresh to retry.
            mark_background_ledger_write_pending(&self.state)
        };
        // The record carries only the bounded category, so the chain that names
        // the ledger and workspace paths is kept here for local diagnosis.
        tracing::warn!(
            record_id,
            exec_session_id,
            error = %error,
            "Background subagent launch failed"
        );
        self.publish_background_status(record_id).await;
        // The caller receives the original spawn error, so a persistence
        // failure must not replace it. The obligation raised above stays owed,
        // and the next refresh retries the write.
        match self.save_background_state().await {
            Ok(()) => clear_background_ledger_write_pending(&self.state, write_generation),
            Err(save_error) => {
                tracing::warn!(
                    record_id,
                    error = %save_error,
                    "Failed to persist the settled background launch failure"
                );
            }
        }
    }

    /// Commits one launch to its record, unless the record no longer wants it.
    ///
    /// The spawn runs without the state lock, so a cancellation, a refresh or
    /// an adopted persisted stop can change the record while it is in flight.
    /// Writing this launch over that change would resurrect a cancelled task,
    /// attach one process's metadata to a different launch, or keep a process
    /// alive that no record names. The commit re-checks the record under the
    /// write lock and reports `false` when it declined, leaving the record as
    /// the concurrent action left it; the caller closes the session it created.
    /// A launch also refuses an exec-session id another record holds: sharing
    /// one id would make the two indistinguishable to pruning and cancellation.
    pub(super) async fn commit_background_run_started(
        &self,
        record_id: &str,
        exec_session_id: &str,
        child_pid: Option<u32>,
        started_at: Option<chrono::DateTime<Utc>>,
    ) -> Result<Option<u64>> {
        let write_generation = {
            let mut state = self.state.write().await;
            let duplicate_session = state
                .background_children
                .iter()
                .any(|(id, other)| id.as_str() != record_id && other.exec_session_id == exec_session_id);
            if duplicate_session {
                return Ok(None);
            }
            let Some(record) = state.background_children.get_mut(record_id) else {
                return Ok(None);
            };
            if !record.desired_enabled || record.exec_session_id != exec_session_id {
                return Ok(None);
            }
            record.pid = child_pid;
            record.started_at = started_at;
            record.status = BackgroundSubprocessStatus::Running;
            record.updated_at = Utc::now();
            record.ended_at = None;
            record.error = None;
            record.summary = Some("Background subagent is running".to_string());
            // The claim this launch reserved is fulfilled, so a concurrent
            // refresh must no longer treat the record as owed a launch.
            clear_background_launch_owed(&self.state, record_id, exec_session_id);
            // The record becomes reusable the moment this lock is released, so
            // the owed-write marker is raised here, under the same lock that
            // stores it. Raising it after the lock let a concurrent reuse
            // answer from memory for a record no other process could see yet.
            Some(mark_background_ledger_write_pending(&self.state))
        };
        self.publish_background_status(record_id).await;
        Ok(write_generation)
    }

    /// Settles a record from a liveness observation.
    ///
    /// Tests and callers with no record phase to report use this entry point.
    /// A caller that read the record before it observed the session uses
    /// [`Self::update_background_record_state_observed`], which can tell a
    /// stale absence from a real one. Production refresh always passes the
    /// phase, so this wrapper is test-only.
    #[cfg(test)]
    pub(super) async fn update_background_record_state(
        &self,
        record_id: &str,
        observed_exec_session_id: Option<&str>,
        snapshot: Option<crate::tools::types::DiapasonExecSession>,
    ) -> Result<Option<(String, String, u8)>> {
        self.update_background_record_state_observed(record_id, observed_exec_session_id, None, snapshot)
            .await
    }

    /// Settles a record from an observation that carries the observed phase.
    ///
    /// `observed_status` is the phase the record had when the caller read it,
    /// which precedes the session observation outside the state lock. A launch
    /// can commit `Running` in between, so a missing session recorded while the
    /// record was `Starting` is stale evidence: acting on it would fail a
    /// launch that is really running, or spend its allowance on a duplicate
    /// replacement. Without the phase the absence is taken at face value.
    pub(super) async fn update_background_record_state_observed(
        &self,
        record_id: &str,
        observed_exec_session_id: Option<&str>,
        observed_status: Option<BackgroundSubprocessStatus>,
        snapshot: Option<crate::tools::types::DiapasonExecSession>,
    ) -> Result<Option<(String, String, u8)>> {
        let mut state = self.state.write().await;
        let Some(record) = state.background_children.get_mut(record_id) else {
            return Ok(None);
        };
        // A stop that is still owed the close of its exec session has already
        // settled its intent, and the reconcile path owns its outcome. Settling
        // it here from a liveness observation would either report the stop
        // complete before the close was attempted or clear the error that makes
        // the pending close visible.
        if background_stop_close_is_pending(&self.state, record_id, &record.exec_session_id) {
            return Ok(None);
        }
        // The observation was made against the session the record named when
        // the refresh read it, outside this lock. A launch that replaced the
        // session in between answers for a run that no longer exists, so its
        // exit or absence must not settle the replacement. The refresh that
        // observed the new session settles that one.
        if observed_exec_session_id != Some(record.exec_session_id.as_str()) {
            return Ok(None);
        }
        // A settled record already carries its terminal outcome, and refresh
        // exists to settle records that are still active. Cancellation clears
        // `desired_enabled` while preserving an `Error` and its message, so
        // letting refresh rewrite it would relabel a failure as `Stopped` —
        // which clients project as "completed" — or relaunch it. Guarding here
        // covers every refresh arm at once.
        if !record.status.is_active() {
            return Ok(None);
        }
        // A `Starting` record that still owes a launch is driven by that
        // launch, or by refresh's retry of a restart a previous pass
        // scheduled. Settling it here would turn a launch still owed into a
        // failure or start a second process beside it. A `Starting` record
        // that owes nothing has no launch left to drive it, so it falls
        // through to the settlement below.
        if record.status == BackgroundSubprocessStatus::Starting
            && background_launch_is_owed(&self.state, record_id, &record.exec_session_id)
        {
            return Ok(None);
        }
        // The absence was observed while the record was `Starting`, and the
        // same launch has since committed `Running`. The session was not
        // registered yet when it was observed, so the absence says nothing
        // about the launch that now owns the record.
        if snapshot.is_none()
            && observed_status == Some(BackgroundSubprocessStatus::Starting)
            && record.status == BackgroundSubprocessStatus::Running
        {
            return Ok(None);
        }
        record.updated_at = Utc::now();

        let Some(snapshot) = snapshot else {
            let respawn = self.handle_missing_background_snapshot(record)?;
            if let Some((agent_name, _, restart_attempts)) = respawn.as_ref() {
                // The restart is recorded before the caller awaits its ledger
                // pre-check, so a dropped refresh can be driven by the next
                // one instead of leaving the record `Starting` forever.
                mark_background_launch_owed(
                    &self.state,
                    record.id.as_str(),
                    agent_name,
                    *restart_attempts,
                    record.exec_session_id.as_str(),
                    true,
                );
            }
            return Ok(respawn);
        };

        record.pid = snapshot.child_pid;
        record.started_at = snapshot.started_at.or(record.started_at);

        match snapshot.lifecycle_state {
            Some(crate::tools::types::DiapasonSessionLifecycleState::Running) => {
                record.status = BackgroundSubprocessStatus::Running;
                record.ended_at = None;
                record.error = None;
            }
            Some(crate::tools::types::DiapasonSessionLifecycleState::Exited) | None => {
                record.ended_at.get_or_insert(Utc::now());
                if record.desired_enabled
                    && restart_allowed(self.config.vt_cfg.subagents.background.auto_restore, record.restart_attempts)
                {
                    let next_restart_attempt = record.restart_attempts.saturating_add(1);
                    record.restart_attempts = next_restart_attempt;
                    record.status = BackgroundSubprocessStatus::Starting;
                    // The previous run has ended, but the replacement has not.
                    // Clearing the instant here keeps every active record from
                    // carrying an end time: the read-only liveness projection
                    // passes `ended_at` through unchanged, so a list that sees
                    // `Starting` before the replacement is installed would
                    // otherwise report an end for a task it also reports as
                    // running.
                    record.ended_at = None;
                    tracing::warn!(
                        agent_name = record.agent_name.as_str(),
                        record_id = record.id.as_str(),
                        attempt = next_restart_attempt,
                        "Background subprocess exited unexpectedly; scheduling restart"
                    );
                    // Recorded under the state lock that moves the record to
                    // `Starting`, before the caller's first await.
                    mark_background_launch_owed(
                        &self.state,
                        record.id.as_str(),
                        record.agent_name.as_str(),
                        next_restart_attempt,
                        record.exec_session_id.as_str(),
                        true,
                    );
                    return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
                }
                self.mark_background_record_stopped_or_error(record, &snapshot);
            }
        }

        Ok(None)
    }

    fn handle_missing_background_snapshot(
        &self,
        record: &mut BackgroundRecord,
    ) -> Result<Option<(String, String, u8)>> {
        // A `Starting` record that still owes a launch is exempted by the
        // caller before it reaches here. Getting here means nothing is left to
        // drive that state: the session the record named never registered or
        // has vanished, so the normal restart and settlement rules below apply
        // instead of leaving the record starting forever.
        if !record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.ended_at.get_or_insert(Utc::now());
            // Refresh skips a terminal record on every later pass, so this
            // outcome exists only in memory until it is written. Register the
            // write under the lock that stores it.
            mark_background_ledger_write_pending(&self.state);
        } else if restart_allowed(self.config.vt_cfg.subagents.background.auto_restore, record.restart_attempts) {
            let next_restart_attempt = record.restart_attempts.saturating_add(1);
            record.restart_attempts = next_restart_attempt;
            record.status = BackgroundSubprocessStatus::Starting;
            // The replacement run has not ended, so it must not inherit the
            // previous run's end instant (see `update_background_record_state`).
            record.ended_at = None;
            tracing::warn!(
                agent_name = record.agent_name.as_str(),
                record_id = record.id.as_str(),
                attempt = next_restart_attempt,
                "Background subprocess is missing; scheduling restart"
            );
            return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
        } else {
            // The run is gone and no retry is allowed: automatic restore is
            // disabled, or the single restart allowance is spent. The record is
            // still expected to be enabled, so the missing process is its
            // failure; leaving it `Running` would report a live task that
            // cannot serve output.
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some("Background subprocess is not running".to_string());
            record.ended_at.get_or_insert(Utc::now());
            mark_background_ledger_write_pending(&self.state);
        }
        Ok(None)
    }

    /// Settles a record whose exec session has exited.
    ///
    /// Both arms write a terminal status and an end instant, and both are
    /// reached from a refresh: the record then exists only in memory, because
    /// refresh skips a terminal record on every later pass and restore filters
    /// it out. The outcome must therefore reach the ledger, so the write
    /// obligation is raised under the same lock that stores it. Without that,
    /// the durable row kept saying `Running` for a task that had ended, and a
    /// resumed session restored a run that could never come back.
    fn mark_background_record_stopped_or_error(
        &self,
        record: &mut BackgroundRecord,
        snapshot: &crate::tools::types::DiapasonExecSession,
    ) {
        if record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some(match snapshot.exit_code {
                Some(exit_code) => format!("Background subprocess exited with code {exit_code}"),
                None => "Background subprocess exited unexpectedly".to_string(),
            });
        } else {
            record.status = BackgroundSubprocessStatus::Stopped;
        }
        record.ended_at.get_or_insert(Utc::now());
        mark_background_ledger_write_pending(&self.state);
    }
}

#[cfg(all(test, unix))]
mod stale_observation_tests;

#[cfg(test)]
mod terminal_refresh_persistence_tests;

#[cfg(test)]
mod terminal_failure_write_tests {
    //! Pins that a settled launch failure stays owed to the ledger until a save
    //! records it.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::subagents::background::{
        background_state_lock_path_for_tests, load_background_state, persist_background_state,
    };
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::*;

    const WORKER: &str = "background-worker";
    const WORKER_AGENT: &str = "worker";
    const LAUNCH_EXEC: &str = "exec-launch";

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

    /// The durable claim a failed launch leaves behind.
    fn starting_record() -> BackgroundRecord {
        BackgroundRecord {
            id: WORKER.to_string(),
            owner_session_id: None,
            agent_name: WORKER_AGENT.to_string(),
            display_label: WORKER_AGENT.to_string(),
            description: WORKER_AGENT.to_string(),
            source: "builtin".to_string(),
            colour: None,
            session_id: format!("session-{WORKER}"),
            parent_session_id: "parent-session".to_string(),
            exec_session_id: LAUNCH_EXEC.to_string(),
            desired_enabled: true,
            status: BackgroundSubprocessStatus::Starting,
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

    /// A terminal settlement whose save fails is retried by the next refresh.
    ///
    /// The record is `Error` only in memory after the failed save; an ordinary
    /// refresh skips a terminal record, so without an owed write the ledger
    /// would keep the unfinished `Starting` claim forever. The settlement must
    /// register the write under the lock that stores the failure, and only a
    /// successful save may clear it.
    ///
    /// The claim is seeded durably first. A retry that can only insert a missing
    /// row would pass this test without ever replacing the claim it exists to
    /// replace, so the `Starting` row must already be in the ledger: the failed
    /// save must leave it there, and the retry must replace it with the
    /// settlement for the same launch.
    #[tokio::test]
    async fn a_terminal_failure_whose_save_fails_stays_owed_to_the_ledger() {
        let temp = TempDir::new().expect("tempdir");
        persist_background_state(temp.path(), vec![starting_record().into_persisted()])
            .await
            .expect("seed the durable Starting claim");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), starting_record());
        }
        let seeded = load_background_state(temp.path()).await.expect("read the seeded ledger");
        assert_eq!(
            seeded
                .records
                .first()
                .map(|record| (record.exec_session_id.as_str(), record.status)),
            Some((LAUNCH_EXEC, BackgroundSubprocessStatus::Starting)),
            "the fixture seeds the unfinished claim the recovery must replace"
        );
        // A directory at the lock path makes the save fail immediately.
        let lock_path = background_state_lock_path_for_tests(temp.path());
        std::fs::create_dir_all(&lock_path).expect("the lock path blocks the save");

        controller
            .settle_failed_background_restart(WORKER, LAUNCH_EXEC, &anyhow::anyhow!("spawn transport fault"))
            .await;

        {
            let state = controller.state.read().await;
            let record = state.background_children.get(WORKER).expect("record");
            assert_eq!(record.status, BackgroundSubprocessStatus::Error);
            // The record's error becomes a client's `stopReason`, so it carries
            // the bounded outermost cause rather than the chain: the chain's
            // context names the workspace and ledger paths, which stay in this
            // process's log. The category is what a caller sees.
            assert!(
                record
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Failed to start background subagent")),
                "{:?}",
                record.error
            );
        }
        assert!(
            pending_background_ledger_write_generation(&controller.state).is_some(),
            "the settlement must owe the ledger the failure it recorded"
        );
        let after_failed_save = load_background_state(temp.path()).await.expect("read the ledger mid-recovery");
        let unfinished = after_failed_save
            .records
            .iter()
            .find(|record| record.id == WORKER)
            .expect("the blocked save leaves the seeded claim in place");
        assert_eq!(unfinished.status, BackgroundSubprocessStatus::Starting, "a failed save must not reach the ledger");
        assert_eq!(unfinished.exec_session_id, LAUNCH_EXEC, "the ledger still names the launch that failed");
        std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

        assert!(
            controller
                .retry_owed_background_ledger_write()
                .await
                .expect("the retried write succeeds"),
            "the failed save must leave a write for the next refresh to retry"
        );
        assert!(
            pending_background_ledger_write_generation(&controller.state).is_none(),
            "a successful write discharges the obligation"
        );
        let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
        let stored = persisted
            .records
            .iter()
            .find(|record| record.id == WORKER)
            .expect("the settled failure reaches the ledger");
        assert_eq!(
            stored.exec_session_id, LAUNCH_EXEC,
            "the settlement replaces the claim for the same launch, not another one"
        );
        assert_eq!(
            stored.status,
            BackgroundSubprocessStatus::Error,
            "the retry replaces the seeded Starting claim with the settled failure"
        );

        controller.signal_shutdown().await;
    }
}
