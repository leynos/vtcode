//! Settles a background record from a launch or a liveness observation.

use super::*;

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
}

/// Applies `update` to the registry, pruning dropped trees first.
fn with_pending_ledger_writes<R>(update: impl FnOnce(&mut Vec<PendingLedgerWrite>) -> R) -> R {
    let mut registry = pending_ledger_writes()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|entry| entry.state.strong_count() > 0);
    update(&mut registry)
}

/// Reports whether `state` is one of the trees in `registry`.
fn tree_has_pending_ledger_write(registry: &[PendingLedgerWrite], state: &Arc<RwLock<ControllerState>>) -> bool {
    registry
        .iter()
        .any(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
}

/// Marks a controller tree as having a record its last save did not write.
pub(super) fn mark_background_ledger_write_pending(state: &Arc<RwLock<ControllerState>>) {
    with_pending_ledger_writes(|registry| {
        if tree_has_pending_ledger_write(registry, state) {
            return;
        }
        registry.push(PendingLedgerWrite { state: Arc::downgrade(state) });
    });
}

/// Reports whether a controller tree still owes the ledger a write.
pub(super) fn background_ledger_write_pending(state: &Arc<RwLock<ControllerState>>) -> bool {
    with_pending_ledger_writes(|registry| tree_has_pending_ledger_write(registry, state))
}

/// Clears the flag after the owed write succeeded.
pub(super) fn clear_background_ledger_write_pending(state: &Arc<RwLock<ControllerState>>) {
    with_pending_ledger_writes(|registry| {
        registry.retain(|entry| !std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)));
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
        if !background_ledger_write_pending(&self.state) {
            return Ok(false);
        }
        self.save_background_state().await?;
        clear_background_ledger_write_pending(&self.state);
        Ok(true)
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
    ) -> Result<bool> {
        {
            let mut state = self.state.write().await;
            let duplicate_session = state
                .background_children
                .iter()
                .any(|(id, other)| id.as_str() != record_id && other.exec_session_id == exec_session_id);
            if duplicate_session {
                return Ok(false);
            }
            let Some(record) = state.background_children.get_mut(record_id) else {
                return Ok(false);
            };
            if !record.desired_enabled || record.exec_session_id != exec_session_id {
                return Ok(false);
            }
            record.pid = child_pid;
            record.started_at = started_at;
            record.status = BackgroundSubprocessStatus::Running;
            record.updated_at = Utc::now();
            record.ended_at = None;
            record.error = None;
            record.summary = Some("Background subagent is running".to_string());
        }
        self.publish_background_status(record_id).await;
        Ok(true)
    }
    pub(super) async fn update_background_record_state(
        &self,
        record_id: &str,
        observed_exec_session_id: Option<&str>,
        snapshot: Option<crate::tools::types::DiapasonExecSession>,
    ) -> Result<Option<(String, String, u8)>> {
        let mut state = self.state.write().await;
        let Some(record) = state.background_children.get_mut(record_id) else {
            return Ok(None);
        };
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
        record.updated_at = Utc::now();

        let Some(snapshot) = snapshot else {
            return Self::handle_missing_background_snapshot(record, &self.config);
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
                    && self.config.vt_cfg.subagents.background.auto_restore
                    && record.restart_attempts < 1
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
                    return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
                }
                Self::mark_background_record_stopped_or_error(record, &snapshot, &self.config);
            }
        }

        Ok(None)
    }

    fn handle_missing_background_snapshot(
        record: &mut BackgroundRecord,
        config: &SubagentControllerConfig,
    ) -> Result<Option<(String, String, u8)>> {
        // A record that is still `Starting` has no registered exec session yet:
        // the launch is in flight, either the first one or a restart this
        // refresh scheduled. Its absence is the expected state of a launch that
        // has not registered, not evidence that the run failed, so the record
        // is left for the launch to settle.
        if record.status == BackgroundSubprocessStatus::Starting {
            return Ok(None);
        }
        if !record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.ended_at.get_or_insert(Utc::now());
        } else if config.vt_cfg.subagents.background.auto_restore && record.restart_attempts < 1 {
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
        }
        Ok(None)
    }

    fn mark_background_record_stopped_or_error(
        record: &mut BackgroundRecord,
        snapshot: &crate::tools::types::DiapasonExecSession,
        _config: &SubagentControllerConfig,
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
    }
}
