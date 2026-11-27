//! Restores and refreshes the background records this controller still desires.

use super::*;

use self::controller_background_cancel::mark_background_stop_close_pending;
use self::controller_background_liveness::{
    BackgroundExecSessionObservation, bounded_liveness_reason, exec_session_is_missing,
};

/// A launch this controller owes but has not committed.
///
/// `update_background_record_state` spends the record's restart allowance and
/// moves it to `Starting` before refresh awaits the restart's ledger
/// pre-check. A refresh dropped inside that await would leave the record
/// `Starting` with nothing left to drive it, so the scheduled restart is
/// recorded here first. The next refresh drives every retryable entry, and a
/// commit or a settlement clears it.
///
/// A fresh claim records the same kind of entry while its spawn is in flight.
/// It is not retried from refresh — the launch that reserved it is already
/// driving it — but it marks the `Starting` record as owned, so a concurrent
/// refresh neither settles that launch nor starts a second process beside it.
struct PendingBackgroundLaunch {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    record_id: String,
    agent_name: String,
    restart_attempts: u8,
    /// The exec session the record named when the launch was scheduled.
    exec_session_id: String,
    /// Whether refresh drives this launch itself.
    ///
    /// A scheduled restart is retryable because the pass that recorded it may
    /// never reach the launch. A fresh claim is not: its reservation is being
    /// spawned by the call that recorded it, and a second launch from refresh
    /// would only race it.
    retry_from_refresh: bool,
}

/// The request fields a refresh retry needs from a [`PendingBackgroundLaunch`].
struct PendingBackgroundRestart {
    record_id: String,
    agent_name: String,
    restart_attempts: u8,
    exec_session_id: String,
}

fn pending_background_launches() -> &'static Mutex<Vec<PendingBackgroundLaunch>> {
    static PENDING: std::sync::OnceLock<Mutex<Vec<PendingBackgroundLaunch>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// Applies `update` to the launch registry, pruning dropped trees first.
fn with_pending_background_launches<R>(update: impl FnOnce(&mut Vec<PendingBackgroundLaunch>) -> R) -> R {
    let mut registry = pending_background_launches()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|entry| entry.state.strong_count() > 0);
    update(&mut registry)
}

/// Reports whether `entry` belongs to the launch identified by the arguments.
fn pending_background_launch_matches(
    entry: &PendingBackgroundLaunch,
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> bool {
    std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
        && entry.record_id == record_id
        && entry.exec_session_id == exec_session_id
}

/// Records that `record_id` owes the launch of `exec_session_id`.
///
/// The first marker for a launch wins: a fresh claim is never promoted into a
/// refresh-driven restart, and a scheduled restart keeps the request details
/// the pass that recorded it resolved.
pub(super) fn mark_background_launch_owed(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    agent_name: &str,
    restart_attempts: u8,
    exec_session_id: &str,
    retry_from_refresh: bool,
) {
    with_pending_background_launches(|registry| {
        if registry
            .iter()
            .any(|entry| pending_background_launch_matches(entry, state, record_id, exec_session_id))
        {
            return;
        }
        registry.push(PendingBackgroundLaunch {
            state: Arc::downgrade(state),
            record_id: record_id.to_owned(),
            agent_name: agent_name.to_owned(),
            restart_attempts,
            exec_session_id: exec_session_id.to_owned(),
            retry_from_refresh,
        });
    });
}

/// Clears the launch obligation for one record's exec session.
///
/// The obligation is what tells a refresh that a launch is still coming, so
/// it is cleared exactly when the launch commits, fails or is abandoned.
/// Clearing one exec session leaves an obligation for a replacement launch
/// under the same record intact.
pub(super) fn clear_background_launch_owed(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) {
    with_pending_background_launches(|registry| {
        registry.retain(|entry| !pending_background_launch_matches(entry, state, record_id, exec_session_id));
    });
}

/// Reports whether `record_id` still owes the launch of `exec_session_id`.
pub(super) fn background_launch_is_owed(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> bool {
    with_pending_background_launches(|registry| {
        registry
            .iter()
            .any(|entry| pending_background_launch_matches(entry, state, record_id, exec_session_id))
    })
}

/// Reports whether a fresh, non-retryable claim still drives this launch.
///
/// A fresh claim is recorded as not retryable because the call that reserved it
/// is about to spawn the process. Its session is not registered yet, so an
/// observation made elsewhere reads it as missing; replacing it would start a
/// second process beside the launch still in flight. A retryable entry is a
/// restart a previous refresh scheduled after its driver was abandoned, so
/// refresh may legitimately drive that one and it is not reported here.
fn background_launch_is_driven(state: &Arc<RwLock<ControllerState>>, record_id: &str, exec_session_id: &str) -> bool {
    with_pending_background_launches(|registry| {
        registry.iter().any(|entry| {
            pending_background_launch_matches(entry, state, record_id, exec_session_id) && !entry.retry_from_refresh
        })
    })
}

/// Returns the scheduled restarts this controller still owes.
fn pending_background_launch_retries(state: &Arc<RwLock<ControllerState>>) -> Vec<PendingBackgroundRestart> {
    with_pending_background_launches(|registry| {
        registry
            .iter()
            .filter(|entry| entry.retry_from_refresh && std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
            .map(|entry| PendingBackgroundRestart {
                record_id: entry.record_id.clone(),
                agent_name: entry.agent_name.clone(),
                restart_attempts: entry.restart_attempts,
                exec_session_id: entry.exec_session_id.clone(),
            })
            .collect()
    })
}

/// A test gate at the restart's ledger pre-check.
///
/// The pre-check is the restart's first await, so it is where a refresh can be
/// dropped after the record was moved to `Starting` with its allowance spent.
/// Arming this gate lets a test drop the restart there deterministically.
#[cfg(test)]
struct BackgroundRestartPreCheckGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
fn background_restart_pre_check_gates() -> &'static Mutex<Vec<BackgroundRestartPreCheckGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<BackgroundRestartPreCheckGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next restart of `state` at its pre-check.
#[cfg(test)]
fn gate_next_background_restart_pre_check(state: &Arc<RwLock<ControllerState>>) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = background_restart_pre_check_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(BackgroundRestartPreCheckGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

#[cfg(test)]
async fn pass_background_restart_pre_check_gate(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = background_restart_pre_check_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| gate.state.strong_count() > 0);
        let index = gates
            .iter()
            .position(|gate| std::ptr::eq(gate.state.as_ptr(), Arc::as_ptr(state)));
        index.map(|index| gates.remove(index))
    };
    let Some(gate) = gate else {
        return;
    };
    gate.entered.notify_one();
    gate.release.notified().await;
}

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
            // The toggle is what a user reads as "the default worker is
            // stopped". A close that could not be confirmed leaves the process
            // possibly live, so the toggle reports a retryable fault instead of
            // a completed stop; the next reconcile retries the close.
            let outcome = self.graceful_stop_background_outcome(TaskOwner::Workspace, &target_id).await?;
            if outcome.is_close_pending() {
                bail!(
                    "Background subagent '{}' stopped accepting work, but its process close could not be confirmed; retry once it settles",
                    agent_name
                );
            }
            Ok(outcome.into_entry())
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
    ///
    /// The same rule decides whether this controller's exec manager can observe
    /// a record's process at all: another session's launch lives in that
    /// process, so a missing read here is not evidence that it exited.
    pub(super) fn background_record_restart_is_authorized(&self, record: &BackgroundRecord) -> bool {
        match self.background_owner_session_id.as_deref() {
            Some(owner) => record.owner_session_id.as_deref() == Some(owner),
            None => record.owner_session_id.is_none(),
        }
    }

    /// Restarts background subagents that were previously enabled but are no longer running.
    pub async fn restore_background_subagents(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        // The close retry needs no ledger read of its own, so it must run even
        // when reconciliation cannot read the ledger: a persistent ledger fault
        // would otherwise leave a cancelled process running forever. The
        // reconciliation fault is re-raised afterwards, before anything is
        // relaunched without the evidence that read was meant to supply.
        let reconciled = self.reconcile_persisted_background_cancellations().await;
        self.retry_pending_background_stop_closes().await?;
        reconciled?;
        let desired_records = {
            let state = self.state.read().await;
            state
                .background_children
                .values()
                .filter(|record| record.desired_enabled && self.background_record_restart_is_authorized(record))
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
            // A fresh claim drives its own launch: the call that reserved it is
            // about to spawn the process. Its exec session is not registered
            // yet, which is not an exit, so restoring here would start a second
            // process beside the launch still in flight. A schedule a dropped
            // refresh left behind is retryable and is driven by refresh instead.
            if background_launch_is_driven(&self.state, &record_id, &exec_session_id) {
                continue;
            }
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
        let reconciled = self.reconcile_persisted_background_cancellations().await;
        self.retry_pending_background_stop_closes().await?;
        reconciled?;
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
            // A fresh claim drives its own launch, so a session that is not
            // registered yet is not an exit. Relaunching would start a second
            // process beside the launch still in flight.
            if background_launch_is_driven(&self.state, &record_id, &exec_session_id) {
                continue;
            }
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

    /// Drives every restart a previous refresh scheduled but never started.
    ///
    /// [`Self::update_background_record_state`] records the restart before
    /// refresh awaits its ledger pre-check, so a refresh dropped inside that
    /// await leaves the obligation here with the record still `Starting`. The
    /// next refresh runs the same restart instead of reporting a launch that
    /// nothing will ever drive. A record that moved on or disappeared while the
    /// obligation waited discharges it unrun: a later observation settles
    /// whatever that record became.
    async fn retry_pending_background_restarts(&self) -> Result<()> {
        for restart in pending_background_launch_retries(&self.state) {
            let still_owed = {
                let state = self.state.read().await;
                state.background_children.get(&restart.record_id).is_some_and(|record| {
                    record.status == BackgroundSubprocessStatus::Starting
                        && record.desired_enabled
                        && record.exec_session_id == restart.exec_session_id
                        && self.background_record_restart_is_authorized(record)
                })
            };
            if !still_owed {
                clear_background_launch_owed(&self.state, &restart.record_id, &restart.exec_session_id);
                continue;
            }
            tracing::info!(
                agent_name = restart.agent_name.as_str(),
                record_id = restart.record_id.as_str(),
                "Retrying a background restart a previous refresh scheduled"
            );
            self.restart_after_background_observation(
                &restart.record_id,
                restart.agent_name,
                restart.restart_attempts,
                Some(restart.exec_session_id.as_str()),
            )
            .await?;
        }
        Ok(())
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
    ///
    /// Every outcome clears the launch obligation the scheduling pass recorded
    /// for the observed exec session. The replacement launch records its own
    /// obligation under its new session, so clearing this one can never hide
    /// the replacement from a concurrent refresh.
    pub(super) async fn restart_after_background_observation(
        &self,
        record_id: &str,
        agent_name: String,
        restart_attempts: u8,
        observed_exec_session_id: Option<&str>,
    ) -> Result<()> {
        if let Some(exec_session_id) = observed_exec_session_id {
            #[cfg(test)]
            pass_background_restart_pre_check_gate(&self.state).await;
            // The record was already moved to `Starting` with its restart
            // allowance spent when refresh scheduled this replacement. A fault
            // reading the ledger must therefore not abandon that state: settle
            // the failed restart for the launch this round observed and report
            // the fault, exactly as a preparation failure is settled below.
            let persisted_stop = match self.persisted_stop_for_launch(record_id).await {
                Ok(persisted_stop) => persisted_stop,
                Err(error) => {
                    self.settle_failed_background_restart(record_id, exec_session_id, &error).await;
                    clear_background_launch_owed(&self.state, record_id, exec_session_id);
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
                    clear_background_launch_owed(&self.state, record_id, exec_session_id);
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
        let result = match self.ensure_background_record_running(request).await {
            Ok(_) => Ok(()),
            Err(error) => {
                // Refresh scheduled this replacement, so its record is
                // `Starting`. If the replacement cannot even be prepared,
                // nothing else drives that state; settle the failure here so
                // the task is not reported as starting forever.
                if let Some(exec_session_id) = observed_exec_session_id {
                    self.settle_failed_background_restart(record_id, exec_session_id, &error).await;
                }
                Err(error)
            }
        };
        if let Some(exec_session_id) = observed_exec_session_id {
            clear_background_launch_owed(&self.state, record_id, exec_session_id);
        }
        result
    }

    /// Refreshes the state of all background subprocesses and respawns as needed.
    pub async fn refresh_background_processes(&self) -> Result<Vec<BackgroundSubprocessEntry>> {
        // The close retry is independent of the ledger read: a process a stop
        // already owes must still be closed when reconciliation cannot read the
        // ledger. The read fault is re-raised below, before any record is
        // settled or respawned from evidence this pass could not verify.
        let reconciled = self.reconcile_persisted_background_cancellations().await;
        self.retry_pending_background_stop_closes().await?;
        reconciled?;
        // A previous refresh can have been dropped between scheduling a
        // restart and awaiting its pre-check, leaving the record `Starting`
        // with its allowance spent. Driving the obligation first gives the
        // record a launch before this pass decides anything from a liveness
        // observation of it.
        self.retry_pending_background_restarts().await?;
        let record_ids = {
            let state = self.state.read().await;
            state.background_children.keys().cloned().collect::<Vec<_>>()
        };

        for record_id in record_ids {
            let restart_authorized = {
                let state = self.state.read().await;
                state
                    .background_children
                    .get(&record_id)
                    .is_some_and(|record| self.background_record_restart_is_authorized(record))
            };
            if !restart_authorized {
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

            // The phase the record had when it was read above is handed to the
            // settlement: the observation is made outside the lock, so the
            // launch can commit `Running` before this write and make a missing
            // session stale evidence.
            let respawn = self
                .update_background_record_state_observed(
                    &record_id,
                    snapshot_target.as_deref(),
                    before_status,
                    snapshot,
                )
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

            // One record's archive fault must not end the pass. The lookup reads
            // an archive another process writes, so a corrupt or unreadable one
            // keeps failing on every refresh while it is repaired; returning
            // here would starve the owed write below and leave a confirmed stop
            // unsaved for as long as that archive stays broken, across restarts.
            // The paths it would have attached are a projection of the record,
            // not the record's outcome, so the fault is reported and the pass
            // carries on.
            if let Err(error) = self.refresh_background_archive_metadata(&record_id).await {
                tracing::warn!(
                    record_id = record_id.as_str(),
                    error = %error,
                    "Could not refresh the background archive metadata; leaving the record as settled"
                );
            }
        }

        // A launch can commit a live record whose save failed; reuse runs
        // refresh first, so the owed write is retried before reuse reports
        // that record as durable.
        self.retry_owed_background_ledger_write().await?;
        // A refresh is the controller's own maintenance pass rather than one
        // caller addressing one task: it already excludes a record another
        // session owns from restart (`background_record_restart_is_authorized`),
        // and it reports the full view it reconciled. It therefore presents
        // the workspace operator's authority to the projection.
        Ok(self.background_status_entries(TaskOwner::Workspace).await)
    }
}

#[cfg(test)]
mod cancellation_reconcile_tests {
    //! Regressions for adopting a persisted stop and settling a failed restart.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::subagents::background::{background_state_path_for_tests, persist_background_state};

    use super::super::controller_background_cancel::{background_stop_close_is_pending, gate_next_background_close};
    use super::super::tests::support::test_controller_config;
    use super::*;

    /// The first adopted record the reconcile closes, in map-key order.
    const REVIEWER: &str = "background-reviewer";
    const REVIEWER_AGENT: &str = "reviewer";
    const REVIEWER_EXEC: &str = "exec-reviewer";
    /// The adopted record whose close a later retry must still owe.
    const WORKER: &str = "background-worker";
    const WORKER_AGENT: &str = "worker";
    const WORKER_EXEC: &str = "exec-worker";

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
            close_fault: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
        }
    }

    /// The same controller configuration with automatic restore enabled.
    fn restarting_config(workspace_root: PathBuf) -> SubagentControllerConfig {
        let mut config = test_controller_config(workspace_root);
        config.vt_cfg.subagents.background.auto_restore = true;
        config
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
        let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
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
    /// spent and owes the launch, so leaving it there after a pre-check fault
    /// would report an active launch forever.
    #[tokio::test]
    async fn a_restart_whose_ledger_pre_check_fails_settles_error() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
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

    /// A refresh dropped at the restart's pre-check is retried by the next one.
    ///
    /// The record was already moved to `Starting` with its restart allowance
    /// spent, and the restart's first await is the ledger pre-check. A refresh
    /// dropped there leaves no launch, so without a recorded obligation the
    /// record would stay `Starting` forever. The next refresh must drive the
    /// restart from the obligation and settle its failure.
    #[tokio::test]
    async fn a_restart_dropped_at_the_pre_check_is_retried_by_the_next_refresh() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(restarting_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        // The fixture agent does not exist, so the retried restart cannot
        // prepare a replacement: only driving the obligation settles the
        // record instead of leaving it `Starting`.
        let record = background_record(WORKER, "no-such-agent", WORKER_EXEC, BackgroundSubprocessStatus::Running, true);
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(record.id.clone(), record);
        }

        // The gated refresh observes the record `Running`, sees its session is
        // missing, schedules the replacement and then reaches the restart's
        // ledger pre-check. Dropping it there is the interrupted pass.
        let (entered, _release) = gate_next_background_restart_pre_check(&controller.state);
        {
            let refresh = controller.refresh_background_processes();
            tokio::pin!(refresh);
            tokio::select! {
                result = &mut refresh => {
                    panic!("the gated refresh settled before it was dropped: {result:?}")
                },
                () = entered.notified() => {}
            }
        }

        {
            let state = controller.state.read().await;
            let record = state.background_children.get(WORKER).expect("record");
            assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
            assert_eq!(record.restart_attempts, 1, "the scheduled restart spent the allowance");
        }
        assert!(
            background_launch_is_owed(&controller.state, WORKER, WORKER_EXEC),
            "a restart dropped at its pre-check must stay owed"
        );

        let error = controller
            .refresh_background_processes()
            .await
            .expect_err("the retried restart cannot resolve its agent");
        assert!(format!("{error:#}").contains("no-such-agent"), "{error:#}");

        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Error,
            "the retried restart must settle its failure instead of staying Starting"
        );
        assert!(
            !background_launch_is_owed(&controller.state, WORKER, WORKER_EXEC),
            "the retry discharges the obligation it drove"
        );
        drop(state);

        controller.signal_shutdown().await;
    }

    /// A restore must not replace a launch its own driver still owes.
    ///
    /// A fresh claim records a non-retryable obligation because the call that
    /// reserved it is about to spawn the process. Until that session registers,
    /// restore observes it as missing, and replacing it would start a second
    /// process beside the launch still in flight. The agent is deliberately
    /// unresolvable: a restore that tried to replace the claim would settle the
    /// record `Error`, so leaving it `Starting` proves the claim was left alone.
    #[tokio::test]
    async fn a_restore_skips_a_record_whose_launch_a_live_driver_still_owes() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(restarting_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        let record =
            background_record(WORKER, "no-such-agent", WORKER_EXEC, BackgroundSubprocessStatus::Starting, true);
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(record.id.clone(), record);
        }
        mark_background_launch_owed(&controller.state, WORKER, "no-such-agent", 1, WORKER_EXEC, false);

        controller
            .restore_background_subagents()
            .await
            .expect("restore leaves the in-flight claim to its driver");

        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "restore must not settle or replace a claim its driver still owns"
        );
        assert_eq!(record.exec_session_id, WORKER_EXEC, "the in-flight launch keeps the record");
        drop(state);
        assert!(
            background_launch_is_owed(&controller.state, WORKER, WORKER_EXEC),
            "the driver's launch obligation is untouched"
        );

        controller.signal_shutdown().await;
    }
}
