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
            self.graceful_stop_background(&target_id).await
        } else {
            self.ensure_background_record_running(BackgroundRunRequest {
                agent_name,
                stable_id: Some(target_id),
                restart_attempts: 0,
                overrides: None,
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

    /// Restarts background subagents that were previously enabled but are no longer running.
    pub async fn restore_background_subagents(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        self.reconcile_persisted_background_cancellations().await?;
        let desired_records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| record.desired_enabled)
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
            self.ensure_background_record_running(BackgroundRunRequest {
                agent_name,
                stable_id: Some(record_id),
                restart_attempts,
                overrides: None,
            })
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
            restored.push(
                self.ensure_background_record_running(BackgroundRunRequest {
                    agent_name,
                    stable_id: Some(record_id),
                    restart_attempts,
                    overrides: None,
                })
                .await?,
            );
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
    async fn reconcile_persisted_background_cancellations(&self) -> Result<()> {
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
            }
        }
        Ok(())
    }

    /// Refreshes the state of all background subprocesses and respawns as needed.
    pub async fn refresh_background_processes(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        self.reconcile_persisted_background_cancellations().await?;
        let record_ids = {
            let state = self.state.read().await;
            state.background_children.keys().cloned().collect::<Vec<_>>()
        };

        let mut changed = false;
        for record_id in record_ids {
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

            let respawn = self.update_background_record_state(&record_id, snapshot).await?;
            let respawning = respawn.is_some();

            if let Some((agent_name, stable_id, restart_attempts)) = respawn {
                self.ensure_background_record_running(BackgroundRunRequest {
                    agent_name,
                    stable_id: Some(stable_id),
                    restart_attempts,
                    overrides: None,
                })
                .await?;
            }

            let changed_this_record = {
                let state = self.state.read().await;
                state.background_children.get(&record_id).is_some_and(|r| {
                    r.status != before_status.unwrap_or(BackgroundSubprocessStatus::Starting) || r.error != before_error
                })
            };
            changed |= changed_this_record;

            if changed_this_record && !respawning {
                self.publish_background_status(&record_id).await;
            }

            self.refresh_background_archive_metadata(&record_id).await?;
        }

        if changed {
            self.save_background_state().await?;
        }
        Ok(self.background_status_entries().await)
    }
}
