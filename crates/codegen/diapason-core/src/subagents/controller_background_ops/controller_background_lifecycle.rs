//! Restores and refreshes the background records this controller still desires.

use super::*;

use self::controller_background_cancel::mark_background_stop_close_pending;
use self::controller_background_liveness::{
    BackgroundExecSessionObservation, bounded_liveness_reason, exec_session_is_missing,
};

impl SubagentController {
    /// Toggles the default background subagent between running and stopped.
    pub async fn toggle_default_background_subagent(&self) -> Result<BackgroundSubprocessEntry> {
        if !self.background_subagents_enabled() {
            bail!("Background subagents are disabled by configuration");
        }

        let agent_name = self
            .configured_default_background_agent()
            .ok_or_else(|| anyhow!("No default background subagent is configured"))?
            .to_string();
        let target_id =
            background_record_id_for_owner(agent_name.as_str(), self.background_owner_session_id.as_deref());
        let should_stop = {
            let state = self.state.read().await;
            state
                .background_children
                .get(&target_id)
                .is_some_and(|record| record.desired_enabled && record.status.is_active())
        };

        if should_stop {
            self.graceful_stop_background(TaskOwner::Workspace, &target_id).await
        } else {
            self.ensure_background_record_running(BackgroundRunRequest {
                agent_name,
                stable_id: Some(target_id),
                restart_attempts: 0,
                overrides: None,
                expected_exec_session_id: None,
            })
            .await
        }
    }

    /// Reads the exec session a background record names, classifying a failure.
    ///
    /// Both callers act on the answer: restore respawns a record it believes
    /// dead, and refresh settles `Error` and schedules a replacement. A read
    /// that fails for a reason other than the session being gone therefore
    /// cannot be reported as "not running", because that would start a second
    /// process for a task that is still alive. Only a typed not-found cause is
    /// an observed exit; every other fault is reported as unobserved, and the
    /// caller leaves the record unchanged for this round.
    async fn observe_background_exec_session(&self, exec_session_id: Option<&str>) -> BackgroundExecSessionObservation {
        let Some(exec_session_id) = exec_session_id.filter(|id| !id.is_empty()) else {
            return BackgroundExecSessionObservation::Missing;
        };
        match self.config.exec_sessions.snapshot_session(exec_session_id).await {
            Ok(snapshot) => BackgroundExecSessionObservation::Live(snapshot),
            Err(error) if exec_session_is_missing(&error) => BackgroundExecSessionObservation::Missing,
            Err(error) => {
                tracing::warn!(
                    reason = bounded_liveness_reason(&error, exec_session_id).as_str(),
                    "Could not read the background exec session; leaving its record unchanged"
                );
                BackgroundExecSessionObservation::Unobserved
            }
        }
    }

    /// Reports whether this controller may restart a background record.
    ///
    /// Restarting is authority over a process, not visibility: a controller may
    /// list every record in the shared ledger, but it may only launch the work
    /// it owns. A scoped controller owns the records tagged with its session. An
    /// unscoped controller — the TUI and CLI — owns the workspace's ownerless
    /// records, including legacy ones. Another session's record belongs to that
    /// session: its exec sessions live in that process, so this controller would
    /// read them as missing and start a second process for the same task.
    fn background_record_restart_is_authorised(&self, record: &BackgroundRecord) -> bool {
        match self.background_owner_session_id.as_deref() {
            Some(owner) => record.owner_session_id.as_deref() == Some(owner),
            None => record.owner_session_id.is_none(),
        }
    }

    /// Restarts background subagents that were previously enabled but are no longer running.
    pub async fn restore_background_subagents(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        self.reconcile_persisted_background_cancellations().await?;
        self.retry_pending_background_stop_closes().await?;
        let desired_records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| record.desired_enabled && self.background_record_restart_is_authorised(record))
                .map(|record| {
                    (
                        record.id.clone(),
                        record.agent_name.clone(),
                        record.exec_session_id.clone(),
                        record.restart_attempts,
                    )
                })
                .collect::<Vec<_>>()
        };

        for (record_id, agent_name, exec_session_id, restart_attempts) in desired_records {
            let observation = self.observe_background_exec_session(Some(exec_session_id.as_str())).await;
            let is_live = matches!(&observation, BackgroundExecSessionObservation::Live(snapshot) if exec_session_is_running(snapshot));

            if is_live || !self.config.vt_cfg.subagents.background.auto_restore {
                continue;
            }
            if matches!(observation, BackgroundExecSessionObservation::Unobserved) {
                continue;
            }
            tracing::info!(
                agent_name = agent_name.as_str(),
                record_id = record_id.as_str(),
                "Restoring background subagent subprocess"
            );
            self.restart_after_background_observation(
                &record_id,
                agent_name,
                restart_attempts,
                Some(exec_session_id.as_str()),
            )
            .await?;
        }

        self.refresh_background_processes().await
    }

    /// Restores only the background records that no session owns.
    ///
    /// The ACP workspace controller loads every record so a session-scoped
    /// controller can be built from the same ledger, but it must never respawn
    /// work another session owns. Legacy records predate ownership and belong
    /// to no session, so they are the only ones it may restore. Unlike
    /// [`Self::restore_background_subagents`] this does not refresh the whole
    /// ledger, because that would settle and respawn records owned by live
    /// sessions.
    pub async fn restore_legacy_background_subagents(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        // A stop another controller persisted after this one loaded its copy is
        // not in memory yet, and relaunching from the stale copy would
        // resurrect it. Reconcile first, as the whole-ledger restore does.
        self.reconcile_persisted_background_cancellations().await?;
        self.retry_pending_background_stop_closes().await?;
        if !self.config.vt_cfg.subagents.background.auto_restore {
            return Ok(Vec::new());
        }
        let desired_records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| record.desired_enabled && record.owner_session_id.is_none())
                .map(|record| {
                    (
                        record.id.clone(),
                        record.agent_name.clone(),
                        record.exec_session_id.clone(),
                        record.restart_attempts,
                    )
                })
                .collect::<Vec<_>>()
        };

        let mut restored = Vec::new();
        for (record_id, agent_name, exec_session_id, restart_attempts) in desired_records {
            let observation = self.observe_background_exec_session(Some(exec_session_id.as_str())).await;
            if matches!(&observation, BackgroundExecSessionObservation::Live(snapshot) if exec_session_is_running(snapshot))
            {
                continue;
            }
            if matches!(observation, BackgroundExecSessionObservation::Unobserved) {
                continue;
            }
            tracing::info!(
                agent_name = agent_name.as_str(),
                record_id = record_id.as_str(),
                "Restoring legacy background subagent subprocess"
            );
            // A relaunch replaces the launch this round observed, so it goes
            // through the same path as a refresh restart: the persisted copy is
            // re-read, and the replacement is bound to the observed session
            // instead of overwriting whatever launch has taken the record.
            self.restart_after_background_observation(
                &record_id,
                agent_name,
                restart_attempts,
                Some(exec_session_id.as_str()),
            )
            .await?;
            restored.push(self.background_status_for(&record_id).await?);
        }
        Ok(restored)
    }

    /// Adopts cancellation intent another controller persisted for this owner.
    ///
    /// An owner-scoped controller keeps its own copy of the ledger, and an
    /// unscoped controller shares that ledger across sessions. When the
    /// unscoped controller cancels one of this owner's tasks it persists
    /// `desired_enabled = false`, but this controller's copy still says the
    /// task is wanted. Refresh would then settle the exited process from the
    /// stale flag and schedule a restart, and the save that follows carries a
    /// newer `updated_at`, so the merge would keep the resurrected copy and
    /// silently undo the stop.
    ///
    /// Reading the persisted cancellation back before refresh selects a
    /// restart keeps the stop. The exec session identifies the launch: an
    /// explicit restart writes a new one, while a stale copy still names the
    /// session the cancellation stopped, so a restart that has already
    /// replaced the session is never mistaken for one to cancel. A concurrent
    /// cancellation that lands after this read is preserved by the save merge,
    /// which also refuses to let a refreshed copy undo a stop for the same
    /// session.
    ///
    /// Adoption only clears the intent; the exec session is closed afterwards
    /// and settles `Stopped` on success. The close is never reported before it
    /// has been attempted, because a live process behind a terminal record is
    /// hidden from the liveness projection.
    pub(super) async fn reconcile_persisted_background_cancellations(&self) -> Result<()> {
        if self.config.managed_background_runtime {
            // A nested scope owns no background records, so its empty view
            // carries no intent to reconcile.
            return Ok(());
        }
        let persisted = controller_persistence::collect_by_canonical_id(
            load_background_state(&self.config.workspace_root).await?.records,
        );
        if persisted.is_empty() {
            return Ok(());
        }
        let mut adopted_launches = Vec::new();
        {
            let mut state = self.state.write().await;
            for (id, record) in state.background_children.iter_mut() {
                let Some(cancelled) = persisted.get(id) else {
                    continue;
                };
                if cancelled.adopt_cancellation_into(record) {
                    tracing::debug!(
                        record_id = id.as_str(),
                        agent_name = record.agent_name.as_str(),
                        "Adopted a background cancellation persisted by another controller"
                    );
                    if !record.exec_session_id.is_empty() {
                        // Raise the obligation for every adopted launch now,
                        // while the write lock still covers the whole
                        // collection, and before any close is awaited. The
                        // record is already no longer desired, so a reconcile
                        // cancelled partway through the closes below would
                        // leave nothing else to retry: `adopt_cancellation_into`
                        // refuses an already-disabled record, refresh skips a
                        // pending close, and restore filters the record out.
                        mark_background_stop_close_pending(&self.state, id.as_str(), record.exec_session_id.as_str());
                        adopted_launches.push((id.clone(), record.exec_session_id.clone()));
                    }
                }
            }
        }
        // The controller that persisted the stop may never have seen the
        // process, because an unscoped controller cannot reach a session in
        // another process, so adopting the stop terminates it here. Closing runs
        // without the state lock, and a session that already exited is expected.
        // Adoption leaves the record active, so a successful close is what
        // settles `Stopped`. A close fault is kept as the stop's obligation: the
        // record stays visibly pending, and a later reconcile retries the close
        // instead of reporting a live process as stopped.
        for (record_id, exec_session_id) in adopted_launches {
            self.attempt_background_stop_close(&record_id, &exec_session_id).await;
            // The close decides the outcome, so publish afterwards. Without
            // this, an observer would never learn that the task stopped (or
            // that its close is still owed): refresh skips a pending close and
            // restore filters the record out once it is no longer desired.
            let still_names_session = {
                let state = self.state.read().await;
                state
                    .background_children
                    .get(&record_id)
                    .is_some_and(|record| record.exec_session_id == exec_session_id)
            };
            if still_names_session {
                self.publish_background_status(&record_id).await;
            }
        }
        Ok(())
    }

    /// Reads the persisted copy of one record under the persistence lock.
    ///
    /// This is the restart path's cheap pre-check: a launch another controller
    /// had already stopped is skipped without preparing a plan at all. It is
    /// only a fast path, so it need not be atomic with the claim — the
    /// authoritative reconciliation is [`SubagentController::reserve_background_run`],
    /// which reads the ledger and writes the claim as one transition.
    ///
    /// The copy is returned unfiltered. Whether it is a stop for this launch is
    /// decided by [`PersistedBackgroundRecord::adopt_cancellation_into`], which
    /// already requires a cleared desired flag and the same exec session; the
    /// session identifies the launch, so a stop for a replaced session leaves
    /// the replacement alone.
    async fn persisted_stop_for_launch(&self, record_id: &str) -> Result<Option<PersistedBackgroundRecord>> {
        if self.config.managed_background_runtime {
            return Ok(None);
        }
        let _persistence_guard = self.lock_scope.persistence().lock().await;
        let mut persisted = controller_persistence::collect_by_canonical_id(
            load_background_state(&self.config.workspace_root).await?.records,
        );
        Ok(persisted.remove(record_id))
    }

    /// Restarts one record after refresh observed its exec session end.
    ///
    /// The observation names the launch this round examined. Another controller
    /// may persist a stop for that launch before the replacement is claimed, so
    /// the claim itself reconciles the ledger: `reserve_background_run` reads
    /// the persisted copy and writes the claim as one transition under the
    /// ledger's file lock, and adopts a matching stop instead of overriding it.
    /// The replacement would otherwise resurrect a task its owner stopped, and
    /// the following save would race that owner's intent. The read here only
    /// short-circuits a plan that is already known to be unwanted.
    pub(super) async fn restart_after_background_observation(
        &self,
        record_id: &str,
        agent_name: String,
        restart_attempts: u8,
        observed_exec_session_id: Option<&str>,
    ) -> Result<()> {
        if let Some(exec_session_id) = observed_exec_session_id {
            // The record was already moved to `Starting` with its restart
            // allowance spent when refresh scheduled this replacement, and the
            // missing-snapshot path deliberately exempts `Starting`. A fault
            // reading the ledger must therefore not abandon that state: settle
            // the failed restart for the launch this round observed and report
            // the fault, exactly as a preparation failure is settled below.
            let persisted_stop = match self.persisted_stop_for_launch(record_id).await {
                Ok(persisted_stop) => persisted_stop,
                Err(error) => {
                    self.settle_failed_background_restart(record_id, exec_session_id, &error).await;
                    return Err(error);
                }
            };
            if let Some(stop) = persisted_stop {
                let adopted = {
                    let mut state = self.state.write().await;
                    state
                        .background_children
                        .get_mut(record_id)
                        .is_some_and(|record| stop.adopt_cancellation_into(record))
                };
                if adopted {
                    tracing::info!(
                        record_id,
                        exec_session_id,
                        "Skipped a background restart; another controller stopped this launch"
                    );
                    // Nothing else closes this launch: the restart is skipped
                    // and the record is no longer desired, so restore will not
                    // select it. The close settles `Stopped` (an already
                    // missing session counts as closed); a fault keeps the
                    // obligation for a later reconcile instead of reporting a
                    // live process as stopped.
                    self.attempt_background_stop_close(record_id, stop.exec_session_id.as_str())
                        .await;
                    self.publish_background_status(record_id).await;
                    return Ok(());
                }
            }
        }
        let request = BackgroundRunRequest {
            agent_name,
            stable_id: Some(record_id.to_string()),
            restart_attempts,
            overrides: None,
            expected_exec_session_id: observed_exec_session_id.map(str::to_owned),
        };
        match self.ensure_background_record_running(request).await {
            Ok(_) => Ok(()),
            Err(error) => {
                // Refresh scheduled this replacement, so its record is
                // `Starting`, which `handle_missing_background_snapshot`
                // deliberately exempts. If the replacement cannot even be
                // prepared, nothing else drives that state; settle the failure
                // here so the task is not reported as starting forever.
                if let Some(exec_session_id) = observed_exec_session_id {
                    self.settle_failed_background_restart(record_id, exec_session_id, &error).await;
                }
                Err(error)
            }
        }
    }

    /// Refreshes the state of all background subprocesses and respawns as needed.
    pub async fn refresh_background_processes(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        self.reconcile_persisted_background_cancellations().await?;
        self.retry_pending_background_stop_closes().await?;
        let record_ids = {
            let state = self.state.read().await;
            state.background_children.keys().cloned().collect::<Vec<_>>()
        };

        for record_id in record_ids {
            let restart_authorised = {
                let state = self.state.read().await;
                state
                    .background_children
                    .get(&record_id)
                    .is_some_and(|record| self.background_record_restart_is_authorised(record))
            };
            if !restart_authorised {
                // Another session owns this record. Its owning process drives
                // its lifecycle, and this controller cannot even see that
                // process's exec sessions, so settling or respawning it here
                // would race the owner and could start a second process under
                // the shared task id. Listing remains workspace-wide.
                continue;
            }
            let (snapshot_target, before_status, before_error) = {
                let state = self.state.read().await;
                let record = state.background_children.get(&record_id);
                (
                    record.map(|r| r.exec_session_id.clone()),
                    record.map(|r| r.status),
                    record.and_then(|r| r.error.clone()),
                )
            };

            let snapshot = match self.observe_background_exec_session(snapshot_target.as_deref()).await {
                BackgroundExecSessionObservation::Live(snapshot) => Some(snapshot),
                BackgroundExecSessionObservation::Missing => None,
                // A read fault is not an exit: settling the record or
                // scheduling a replacement would start a second process for a
                // task that may still be running.
                BackgroundExecSessionObservation::Unobserved => continue,
            };

            let respawn = self
                .update_background_record_state(&record_id, snapshot_target.as_deref(), snapshot)
                .await?;
            let respawning = respawn.is_some();

            if let Some((agent_name, stable_id, restart_attempts)) = respawn {
                self.restart_after_background_observation(
                    &stable_id,
                    agent_name,
                    restart_attempts,
                    snapshot_target.as_deref(),
                )
                .await?;
            }

            let changed_this_record = {
                let state = self.state.read().await;
                state.background_children.get(&record_id).is_some_and(|r| {
                    r.status != before_status.unwrap_or(BackgroundSubprocessStatus::Starting) || r.error != before_error
                })
            };
            if changed_this_record && !respawning {
                self.publish_background_status(&record_id).await;
            }

            self.refresh_background_archive_metadata(&record_id).await?;
        }

        // A launch can commit a live record whose save failed; reuse runs
        // refresh first, so the owed write is retried before reuse reports
        // that record as durable.
        self.retry_owed_background_ledger_write().await?;
        Ok(self.background_status_entries().await)
    }
}

#[cfg(test)]
mod cancellation_reconcile_tests {
    //! Regressions for adopting a persisted stop and settling a failed restart.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::subagents::background::{background_state_path_for_tests, persist_background_state};
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::super::controller_background_cancel::{background_stop_close_is_pending, gate_next_background_close};
    use super::*;

    /// The first adopted record the reconcile closes, in map-key order.
    const REVIEWER: &str = "background-reviewer";
    const REVIEWER_AGENT: &str = "reviewer";
    const REVIEWER_EXEC: &str = "exec-reviewer";
    /// The adopted record whose close a later retry must still owe.
    const WORKER: &str = "background-worker";
    const WORKER_AGENT: &str = "worker";
    const WORKER_EXEC: &str = "exec-worker";

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

    fn background_record(
        id: &str,
        agent_name: &str,
        exec_session_id: &str,
        status: BackgroundSubprocessStatus,
        desired_enabled: bool,
    ) -> BackgroundRecord {
        BackgroundRecord {
            id: id.to_string(),
            owner_session_id: None,
            agent_name: agent_name.to_string(),
            display_label: agent_name.to_string(),
            description: agent_name.to_string(),
            source: "builtin".to_string(),
            colour: None,
            session_id: format!("session-{id}"),
            parent_session_id: "parent-session".to_string(),
            exec_session_id: exec_session_id.to_string(),
            desired_enabled,
            status,
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

    fn persisted_stop(id: &str, agent_name: &str, exec_session_id: &str) -> PersistedBackgroundRecord {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "owner_session_id": null,
            "agent_name": agent_name,
            "display_label": agent_name,
            "description": agent_name,
            "source": "builtin",
            "color": null,
            "session_id": format!("session-{id}"),
            "exec_session_id": exec_session_id,
            "desired_enabled": false,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": null,
            "pid": null,
            "prompt": "work",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }))
        .expect("persisted background record")
    }

    /// Every adopted launch is owed its close before the first one is awaited.
    ///
    /// Adoption has already cleared `desired_enabled`, and
    /// `adopt_cancellation_into` refuses an already-disabled record, so a
    /// reconcile cancelled inside the first close could never retry a later
    /// adopted launch left behind. Each adopted launch must register its
    /// obligation while the whole set is collected, before any close is
    /// awaited, and a later retry must finish them all.
    #[tokio::test]
    async fn an_aborted_reconcile_still_owes_every_adopted_close() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        let records = [
            background_record(REVIEWER, REVIEWER_AGENT, REVIEWER_EXEC, BackgroundSubprocessStatus::Running, true),
            background_record(WORKER, WORKER_AGENT, WORKER_EXEC, BackgroundSubprocessStatus::Running, true),
        ];
        {
            let mut state = controller.state.write().await;
            for record in records {
                state.background_children.insert(record.id.clone(), record);
            }
        }
        persist_background_state(
            temp.path(),
            vec![
                persisted_stop(REVIEWER, REVIEWER_AGENT, REVIEWER_EXEC),
                persisted_stop(WORKER, WORKER_AGENT, WORKER_EXEC),
            ],
        )
        .await
        .expect("seed the stopped ledger");

        // The map is keyed in id order, so the reviewer's close runs first.
        let (entered, _release) = gate_next_background_close(&controller.state, REVIEWER_EXEC);
        {
            let reconcile = controller.reconcile_persisted_background_cancellations();
            tokio::pin!(reconcile);
            tokio::select! {
                result = &mut reconcile => {
                    panic!("the gated close settled before the reconcile was aborted: {result:?}")
                },
                () = entered.notified() => {}
            }
        }

        assert!(
            background_stop_close_is_pending(&controller.state, REVIEWER, REVIEWER_EXEC),
            "the close that was in flight must stay owed"
        );
        assert!(
            background_stop_close_is_pending(&controller.state, WORKER, WORKER_EXEC),
            "a launch whose close was never attempted must still be owed"
        );

        controller
            .retry_pending_background_stop_closes()
            .await
            .expect("the later reconcile closes both launches");
        {
            let state = controller.state.read().await;
            for id in [REVIEWER, WORKER] {
                let record = state.background_children.get(id).expect("record");
                assert_eq!(
                    record.status,
                    BackgroundSubprocessStatus::Stopped,
                    "{id} must be settled by its retried close"
                );
            }
        }
        let state = &controller.state;
        let reviewer_owed = background_stop_close_is_pending(state, REVIEWER, REVIEWER_EXEC);
        let worker_owed = background_stop_close_is_pending(state, WORKER, WORKER_EXEC);
        assert!(!reviewer_owed && !worker_owed, "a successful retry discharges both close obligations");

        controller.signal_shutdown().await;
    }

    /// A ledger fault while checking for a persisted stop settles the restart.
    ///
    /// The record has already been moved to `Starting` with its allowance
    /// spent, and the missing-snapshot path exempts `Starting`, so leaving it
    /// there after a pre-check fault would report an active launch forever.
    #[tokio::test]
    async fn a_restart_whose_ledger_pre_check_fails_settles_error() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        let record = background_record(WORKER, WORKER_AGENT, WORKER_EXEC, BackgroundSubprocessStatus::Starting, true);
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(record.id.clone(), record);
        }
        let ledger_path = background_state_path_for_tests(temp.path());
        std::fs::create_dir_all(ledger_path.parent().expect("ledger parent")).expect("state dir");
        std::fs::write(&ledger_path, "{ this is not a valid background ledger").expect("write corrupt ledger");

        let error = controller
            .restart_after_background_observation(WORKER, WORKER_AGENT.to_string(), 1, Some(WORKER_EXEC))
            .await
            .expect_err("a ledger fault must reach the caller");

        assert!(format!("{error:#}").contains("Failed to parse"), "the caller sees the ledger fault: {error:#}");
        {
            let state = controller.state.read().await;
            let record = state.background_children.get(WORKER).expect("record");
            assert_eq!(
                record.status,
                BackgroundSubprocessStatus::Error,
                "a restart whose pre-check could not read the ledger must not stay Starting"
            );
            assert!(
                record
                    .error
                    .as_deref()
                    .is_some_and(|error| error.contains("Failed to start background subagent")),
                "{:?}",
                record.error
            );
            assert!(record.ended_at.is_some(), "a settled restart failure has ended");
        }

        controller.signal_shutdown().await;
    }
}
