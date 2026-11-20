//! Restores and refreshes the background records this controller still desires.

use super::*;

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
        if let Some(exec_session_id) = observed_exec_session_id
            && let Some(stop) = self.persisted_stop_for_launch(record_id).await?
        {
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
                // Nothing else closes this launch: the restart is skipped and
                // the record is no longer desired, so restore will not select
                // it. The close settles `Stopped` (an already missing session
                // counts as closed); a fault keeps the obligation for a later
                // reconcile instead of reporting a live process as stopped.
                self.attempt_background_stop_close(record_id, stop.exec_session_id.as_str())
                    .await;
                self.publish_background_status(record_id).await;
                return Ok(());
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
