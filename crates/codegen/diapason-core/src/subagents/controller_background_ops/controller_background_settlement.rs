//! Settles a background record from a launch or a liveness observation.

use super::*;

use super::controller_background_cancel::background_stop_close_is_pending;
use super::controller_background_lifecycle::{
    background_launch_is_owed, clear_background_launch_owed, mark_background_launch_owed,
};

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
            let respawn = Self::handle_missing_background_snapshot(record, &self.config)?;
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
                Self::mark_background_record_stopped_or_error(record, &snapshot, &self.config);
            }
        }

        Ok(None)
    }

    fn handle_missing_background_snapshot(
        record: &mut BackgroundRecord,
        config: &SubagentControllerConfig,
    ) -> Result<Option<(String, String, u8)>> {
        // A `Starting` record that still owes a launch is exempted by the
        // caller before it reaches here. Getting here means nothing is left to
        // drive that state: the session the record named never registered or
        // has vanished, so the normal restart and settlement rules below apply
        // instead of leaving the record starting forever.
        if !record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.ended_at.get_or_insert(Utc::now());
        } else if restart_allowed(config.vt_cfg.subagents.background.auto_restore, record.restart_attempts) {
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

#[cfg(all(test, unix))]
mod stale_observation_tests {
    //! Pins the phase check that discards an absence observed before a commit.

    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::super::BackgroundRunRequest;
    use super::super::controller_background_run::gate_next_background_commit;
    use super::*;

    /// Writes the demo agent and a script that keeps its process alive.
    fn write_demo_agent(workspace_root: &std::path::Path) {
        let agent_dir = workspace_root.join(diapason_commons::workspace_dir_name()).join("agents");
        std::fs::create_dir_all(&agent_dir).expect("agent dir");
        std::fs::write(
            agent_dir.join("background-demo.md"),
            r#"---
name: background-demo
description: Minimal demo agent for the managed background subprocess flow.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background demo.
"#,
        )
        .expect("write background agent");

        let scripts_dir = workspace_root.join("scripts");
        std::fs::create_dir_all(&scripts_dir).expect("scripts dir");
        let script_path = scripts_dir.join("demo-background-subagent.sh");
        std::fs::write(&script_path, "#!/bin/sh\nsleep 30\n").expect("write background script");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("background script permissions");
    }

    fn test_config(workspace_root: PathBuf, vt_cfg: DiapasonConfig) -> SubagentControllerConfig {
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

    /// A missing session observed while `Starting` is discarded after the same
    /// launch commits.
    ///
    /// Refresh reads the record and then observes its exec session outside the
    /// lock. A spawn that has not registered yet reads as missing, and the
    /// launch can commit `Running` before that observation is applied. Acting
    /// on the stale absence would fail the launch, or spend its allowance on a
    /// duplicate replacement. The committed launch is produced by the existing
    /// commit gate, so the observation's phase is the one a refresh really sees
    /// while the spawn is still in flight.
    #[tokio::test]
    async fn a_missing_observation_from_before_the_commit_does_not_settle_the_launch() {
        let temp = TempDir::new().expect("tempdir");
        write_demo_agent(temp.path());
        let mut vt_cfg = DiapasonConfig::default();
        vt_cfg.subagents.background.enabled = true;
        vt_cfg.subagents.background.auto_restore = true;
        let controller = SubagentController::new(test_config(temp.path().to_path_buf(), vt_cfg))
            .await
            .expect("controller");

        let (entered, release) = gate_next_background_commit(&controller.state);
        let launch = controller.ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: Some("background-worker".to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        });
        tokio::pin!(launch);
        let (exec_session_id, observed_status) = tokio::select! {
            result = &mut launch => {
                panic!("the gated launch settled before its observation: {result:?}")
            },
            () = entered.notified() => {
                let state = controller.state.read().await;
                let record = state.background_children.get("background-worker").expect("record");
                assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
                (record.exec_session_id.clone(), record.status)
            }
        };

        release.notify_one();
        let entry = launch.await.expect("the launch commits");
        assert_eq!(entry.status, BackgroundSubprocessStatus::Running);

        let respawn = controller
            .update_background_record_state_observed(
                "background-worker",
                Some(exec_session_id.as_str()),
                Some(observed_status),
                None,
            )
            .await
            .expect("a stale absence is not a settlement failure");
        assert!(respawn.is_none(), "a stale absence must not schedule a replacement");

        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Running,
            "the launch that committed must not be settled from an absence observed before it"
        );
        assert_eq!(record.restart_attempts, 0, "a stale absence must not spend the restart allowance");
        drop(state);

        controller.signal_shutdown().await;
    }
}
