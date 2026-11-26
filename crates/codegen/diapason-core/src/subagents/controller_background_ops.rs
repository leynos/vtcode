//! Manages background subagent work, including task completion, cancellation, and archived results.

mod controller_background_archive;
mod controller_background_cancel;
mod controller_background_launch;
mod controller_background_lifecycle;
mod controller_background_liveness;
mod controller_background_query;
mod controller_background_run;
mod controller_background_settlement;
mod reservation;

#[cfg(test)]
mod adopted_stop_gate;

#[cfg(test)]
mod tests;

use super::*;

pub use self::controller_background_cancel::BackgroundStopOutcome;
pub(in crate::subagents) use self::controller_background_cancel::rekey_pending_background_stop_close;
use self::controller_background_cancel::{background_record_close_is_unconfirmed, background_stop_close_is_pending};
use self::controller_background_lifecycle::mark_background_launch_owed;
use self::controller_background_liveness::exec_session_is_missing;
use self::controller_background_query::{read_output_tail, read_preview};
use self::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
    persisted_background_row_is_desired,
};

pub(in crate::subagents) use self::reservation::{BackgroundRunPlan, BackgroundRunRequest, BackgroundRunReservation};
use self::reservation::{
    PersistedLaunchEvidence, ReservedClaimGuard, background_launch_conflicts, reuse_is_for_another_parent,
    reuse_is_refused_by_the_invocation_context, reuse_needs_parent_rebind, starting_background_record,
    usable_background_parent_session_id,
};

/// Disarms a reservation's claim guard when the claim has a new owner.
///
/// The guard is only meaningful while the reservation is running; every path
/// that settles the claim itself, or hands it to the launch, disarms it here.
fn claim_guard_disarm(guard: Option<ReservedClaimGuard>) {
    if let Some(guard) = guard {
        guard.disarm();
    }
}

/// How an explicit start's admission checks answered.
///
/// The variants are ranked, and the ranking is the rule: an unconfirmed close
/// is decided before the exemption for this controller's own row, because a
/// close this process still owes names a process that may be live whoever holds
/// the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExplicitStartAdmission {
    /// Neither the ledger nor this controller's obligations block the start.
    Admitted,
    /// A persisted row's launch is not confirmed closed, so a process may be live.
    CloseUnconfirmed,
    /// A desired ledger row names a launch this controller does not hold.
    CompetingClaim,
    /// This controller's own stop has not finished closing its process.
    StopClosePending,
}

/// Decides whether an explicit start may claim the record it addresses.
///
/// The checks run in the reservation, under the ledger's file lock and the
/// state write lock, so the row read here is the row another start would have to
/// overwrite. Kept as a predicate over that state so the precedence can be
/// exercised without writing the ledger or publishing a record.
fn explicit_start_admission(
    state_handle: &Arc<RwLock<ControllerState>>,
    state: &ControllerState,
    persisted: Option<&PersistedBackgroundRecord>,
    record_id: &str,
    exec_session_id: &str,
) -> ExplicitStartAdmission {
    if let Some(row) = persisted {
        // A failure whose process close is unconfirmed names a process that may
        // still be live, so its row is not a finished launch whoever holds it.
        // This check comes before the own-record exemption below: a controller's
        // own settled row with a close still owed is exactly the case that must
        // not be replaced.
        if background_record_close_is_unconfirmed(&BackgroundRecord::from_persisted(row.clone())) {
            return ExplicitStartAdmission::CloseUnconfirmed;
        }
        // A desired row naming a launch this controller does not hold is another
        // process's claim. The explicit-start pre-check reads the same row, but
        // only before this lock is taken; two starts that both saw an empty
        // ledger could otherwise reserve and spawn, leaving one launch in the
        // ledger and two processes running. A stopped row is not a claim - an
        // explicit start is how a stopped task is started again - and a row
        // naming this controller's own record, or the launch this start
        // proposes, is this launch's own copy.
        let own_exec_session_id = state
            .background_children
            .get(record_id)
            .map(|record| record.exec_session_id.as_str());
        if Some(row.exec_session_id.as_str()) != own_exec_session_id
            && row.exec_session_id != exec_session_id
            && persisted_background_row_is_desired(row)
        {
            return ExplicitStartAdmission::CompetingClaim;
        }
    }
    // A stop is not finished until its process is closed. Replacing the record
    // now would publish a new launch over a process no record names, and clear
    // the obligation that a later reconcile still needs to discharge.
    if state
        .background_children
        .get(record_id)
        .is_some_and(|record| background_stop_close_is_pending(state_handle, record_id, &record.exec_session_id))
    {
        return ExplicitStartAdmission::StopClosePending;
    }
    ExplicitStartAdmission::Admitted
}

impl SubagentController {
    /// Claims the record for `plan`, deciding and writing under one lock.
    ///
    /// `reusable_background_subprocess` answers the same question without the
    /// state lock, and the launch then wrote its record unconditionally, so
    /// two launches that both passed could both spawn and the second silently
    /// replaced the first — possibly a running one. Deciding and writing here
    /// makes the record's owner a single fact. A plan bound to an observed
    /// exec session may replace only that launch. An unbound plan reuses an
    /// active record that matches its settings, refuses one that does not, and
    /// replaces only an absent or terminal record.
    ///
    /// A bound plan is always an automatic replacement, so it also yields to a
    /// stop another controller has persisted for the launch it observed. The
    /// stop is read and the claim written while the ledger's file lock is held,
    /// because the two must be one transition: reading outside it leaves a
    /// window in which a stop lands after the read, and the replacement then
    /// launches a task its owner had already cancelled. An explicit start is
    /// unbound, so it can still launch after a stop.
    ///
    /// A fresh claim is written to the ledger before the lock is released.
    /// Committing it only in memory would let another process read the record
    /// as unchanged and reserve its own replacement, so the claim is the one
    /// fact both processes must agree on. A bound replacement then confirms the
    /// ledger actually names the claimed exec session: the save merges rather
    /// than replaces, and a claim that lost that merge would let another process
    /// start a replacement beside this launch.
    ///
    /// An explicit start is checked against the same ledger under the same lock.
    /// The pre-check before the reservation reads the ledger too, but only
    /// before this lock is taken, so two starts that both saw an empty ledger
    /// could otherwise reserve and spawn; deciding here leaves only one of them
    /// able to spawn. A ledger that cannot be locked fails the start outright:
    /// a memory-only claim would reserve the task without ever reading the row
    /// another process may hold. The claim is then guarded until the launch path
    /// takes it over, so a caller dropped inside the reservation settles the
    /// claim it wrote instead of leaving it `Starting` forever.
    async fn reserve_background_run(&self, plan: &BackgroundRunPlan) -> Result<BackgroundRunReservation> {
        // A record whose ledger write is still owed must not be answered from
        // memory. Reuse would report a `Running` record that a restart or
        // another process may never see. Retrying here makes the reservation
        // either wait for that write or fail visibly; it can never return
        // `Reused` while the write is outstanding.
        if pending_background_ledger_write_generation(&self.state).is_some() {
            self.retry_owed_background_ledger_write().await?;
        }
        // The file lock is taken before the state lock and released before
        // anything publishes, matching `save_background_state`'s order. It is
        // held for the state decision and for the claim's own write, so two
        // processes cannot both reserve the same record.
        let ledger_guard = if self.config.managed_background_runtime {
            // A nested scope owns no ledger, so it has nothing to read or write.
            None
        } else {
            // Both kinds of claim are one durable transition with the row they
            // reconcile, so a contended or unreadable ledger fails them. An
            // explicit start used to fall back to a memory-only claim, which
            // reserved the task without ever reading the row another process
            // may hold: the start then spawned beside a launch the ledger
            // already named. Propagating the fault leaves the ledger untouched
            // and the caller free to retry, which is what lets a retry inspect
            // the winning claim.
            Some(lock_background_state(&self.config.workspace_root).await?)
        };
        // The ledger is read under the lock that writes the claim. A bound plan
        // compares its observation with the row another process may have
        // written; an unbound plan observed no launch of its own, so the row is
        // the only way a claim another start wrote can be seen.
        let evidence = if ledger_guard.is_some() {
            self.persisted_record_for(&plan.record_id, plan.observed_exec_session_id())
                .await?
        } else {
            PersistedLaunchEvidence::default()
        };
        // A migrated row is evidence about the launch a plan observed. An unbound
        // plan observed none, so a row that merely moved cannot decline a start
        // that replaces nothing.
        let launch_moved = evidence.observed_launch_moved && plan.expected_exec_session_id.is_some();
        let launch_superseded = evidence.observed_launch_superseded;
        let persisted = evidence.record;
        let (reservation, adopted_exec_session_id, claim_is_durable, claim_guard) = {
            let mut state = self.state.write().await;
            // An explicit start is unbound, so nothing ties it to the row the
            // ledger holds: its admission is one decision over that row and this
            // controller's obligations. A bound plan replaces the launch it
            // observed and compares the row directly instead.
            let admission = if plan.expected_exec_session_id.is_some() {
                ExplicitStartAdmission::Admitted
            } else {
                explicit_start_admission(
                    &self.state,
                    &state,
                    persisted.as_ref(),
                    plan.record_id.as_str(),
                    plan.exec_session_id.as_str(),
                )
            };
            if let Some(expected_exec_session_id) = plan.expected_exec_session_id.as_deref() {
                let still_owns_launch = state
                    .background_children
                    .get(&plan.record_id)
                    .is_some_and(|record| record.exec_session_id == expected_exec_session_id && record.desired_enabled);
                // A persisted row under this id that still wants a different
                // launch is another process's claim, not a stale copy of the
                // launch this plan observed. Its storage is disambiguated only
                // by the exec session, so replacing it would put a second
                // process beside a claim this controller cannot see.
                let competing_claim = persisted.as_ref().is_some_and(|row| {
                    row.exec_session_id != expected_exec_session_id && persisted_background_row_is_desired(row)
                });
                // The launch this plan observed is no longer here under this
                // id, or it was migrated to its owner and is now persisted
                // elsewhere. Either way it is not this controller's to
                // replace: spawning now would put a second process beside the
                // migrated record. A migration whose owner has since replaced
                // the launch leaves no row naming the observed session, so the
                // retained migration evidence is what refuses the stale restart.
                if !still_owns_launch || launch_moved || launch_superseded || competing_claim {
                    (BackgroundRunReservation::Declined, None, false, None)
                } else {
                    // `adopt_cancellation_into` is the one place that decides
                    // whether a persisted copy is a stop of this launch.
                    let adopted = match (state.background_children.get_mut(&plan.record_id), persisted.as_ref()) {
                        (Some(record), Some(stop)) => {
                            if stop.adopt_cancellation_into(record) {
                                Some(record.exec_session_id.clone())
                            } else {
                                None
                            }
                        }
                        _ => None,
                    };
                    if let Some(exec_session_id) = adopted {
                        // Capture the adopted launch's session here, while the
                        // record still names it. A concurrent start can replace
                        // the record before the close below, and closing
                        // whatever the record names then would close the
                        // replacement instead.
                        (BackgroundRunReservation::Declined, Some(exec_session_id), false, None)
                    } else {
                        state
                            .background_children
                            .insert(plan.record_id.clone(), starting_background_record(plan));
                        mark_background_launch_owed(
                            &self.state,
                            plan.record_id.as_str(),
                            plan.spec.name.as_str(),
                            plan.restart_attempts,
                            plan.exec_session_id.as_str(),
                            false,
                        );
                        // Armed at the claim write, before the reservation's
                        // first await, so a caller dropped while the claim is
                        // written or published still settles it.
                        let guard = ReservedClaimGuard::arm(
                            &self.state,
                            plan.record_id.as_str(),
                            plan.exec_session_id.as_str(),
                        );
                        (BackgroundRunReservation::Reserved, None, ledger_guard.is_some(), Some(guard))
                    }
                }
            } else if admission == ExplicitStartAdmission::CloseUnconfirmed {
                bail!(
                    "Background subprocess '{}' still owes the close of its previous launch; refresh the background tasks before starting it",
                    plan.spec.name
                );
            } else if admission == ExplicitStartAdmission::CompetingClaim {
                bail!(
                    "Background subprocess '{}' already has a launch claimed by another controller; refresh the background tasks before starting it",
                    plan.spec.name
                );
            } else if admission == ExplicitStartAdmission::StopClosePending {
                (BackgroundRunReservation::Declined, None, false, None)
            } else {
                let active = state.background_children.get(&plan.record_id).and_then(|record| {
                    (record.desired_enabled && record.status.is_active()).then(|| {
                        let conflicts = background_launch_conflicts(
                            record,
                            plan.prompt.as_str(),
                            plan.max_turns,
                            plan.model_override.as_deref(),
                            plan.reasoning_override.as_deref(),
                        );
                        (conflicts, record.build_status_entry(), record.parent_session_id.clone())
                    })
                });
                let (reservation, claim_is_durable, guard) = match active {
                    Some((conflicts, entry, parent_session_id)) => {
                        // Reuse answers with the existing launch and drops the
                        // parent this request resolved, so a record another
                        // parent drives would keep routing its progress there
                        // while the caller is told its own request succeeded.
                        // The same rule applies here, where the decision is
                        // really made, and it compares the resolved parent as
                        // well as the ambient invocation context: a caller with
                        // no context must not reuse a foreign launch either. A
                        // parentless record has no owner to conflict with, so
                        // both comparisons exempt it; the rebind below is what
                        // takes it onto the requesting session.
                        if reuse_is_for_another_parent(&parent_session_id, &plan.parent_session_id)
                            || reuse_is_refused_by_the_invocation_context(&parent_session_id)
                        {
                            bail!("A background subprocess with this agent name is already active");
                        }
                        if conflicts.is_empty() {
                            // A record that names no parent routes no progress
                            // to the caller, so reuse takes the launch onto the
                            // requesting session. The write lock is held here,
                            // and the owed-write marker makes the rebind durable
                            // through the next save or reuse.
                            if reuse_needs_parent_rebind(&parent_session_id, &plan.parent_session_id) {
                                if let Some(record) = state.background_children.get_mut(&plan.record_id) {
                                    record.parent_session_id = plan.parent_session_id.clone();
                                    record.updated_at = Utc::now();
                                }
                                mark_background_ledger_write_pending(&self.state);
                            }
                            (BackgroundRunReservation::Reused(Box::new(entry)), false, None)
                        } else {
                            bail!(
                                "spawn_background_subprocess found active background subprocess '{}' with different {}. Stop or restart the existing subprocess before changing its launch settings.",
                                plan.spec.name,
                                conflicts.join(", "),
                            )
                        }
                    }
                    None => {
                        state
                            .background_children
                            .insert(plan.record_id.clone(), starting_background_record(plan));
                        mark_background_launch_owed(
                            &self.state,
                            plan.record_id.as_str(),
                            plan.spec.name.as_str(),
                            plan.restart_attempts,
                            plan.exec_session_id.as_str(),
                            false,
                        );
                        // Armed at the claim write, before the reservation's
                        // first await, so a caller dropped while the claim is
                        // written or published still settles it.
                        let guard = ReservedClaimGuard::arm(
                            &self.state,
                            plan.record_id.as_str(),
                            plan.exec_session_id.as_str(),
                        );
                        (BackgroundRunReservation::Reserved, ledger_guard.is_some(), Some(guard))
                    }
                };
                (reservation, None, claim_is_durable, guard)
            }
        };
        // The claim is persisted while the ledger lock is still held, so another
        // process cannot read the previous row and reserve its own replacement
        // in the window between this decision and its write. The ordinary save
        // would re-lock the ledger, so the row is written directly here.
        let claim_write_error = if claim_is_durable {
            match self
                .persist_reserved_background_claim(&plan.record_id, &plan.exec_session_id)
                .await
            {
                // A bound replacement reports `Reserved` only once the ledger
                // holds the claim. The save merges rather than replaces, and a
                // claim can lose that merge - a persisted launch that recorded
                // its start outranks a claim that has not begun - leaving
                // `Ok(())` for a write that kept another row. Anyone reading the
                // ledger would then see a launch this controller is about to
                // start beside.
                Ok(()) if plan.expected_exec_session_id.is_some() => {
                    match self.persisted_claim_names(&plan.exec_session_id).await {
                        Ok(true) => None,
                        Ok(false) => Some(anyhow!(
                            "The background ledger did not keep the claim for '{}'; another launch is persisted under it",
                            plan.spec.name
                        )),
                        Err(error) => Some(error),
                    }
                }
                Ok(()) => None,
                Err(error) => Some(error),
            }
        } else {
            None
        };
        if let Some(error) = claim_write_error {
            // No ledger-backed start may spawn a process the ledger cannot name:
            // another controller would read the previous row and start its own,
            // and a later save cannot undo either process. The claim is
            // withdrawn so the caller reports the fault instead of publishing a
            // launch no durable row records. A nested scope owns no ledger, so
            // it never reaches this: its claim is memory-only by construction.
            //
            // The settlement saves the record, so the ledger lock must be
            // released first: the save takes the same lock. The guard is disarmed
            // because the settlement, not the drop handler, settles the claim it
            // wrote.
            drop(ledger_guard);
            claim_guard_disarm(claim_guard);
            self.settle_failed_background_restart(&plan.record_id, &plan.exec_session_id, &error)
                .await;
            return Err(error);
        }
        // The ledger lock is released before publishing: observers run user
        // code, which must not hold a lock every save in the workspace needs.
        drop(ledger_guard);
        let adopted_stop = adopted_exec_session_id.is_some();
        if let Some(adopted_exec_session_id) = adopted_exec_session_id {
            // Adoption only clears the intent; the record stays active until
            // its close succeeds. Close the launch whose stop was adopted, not
            // whatever the record names by now: another start can take the
            // record while the ledger lock is released.
            #[cfg(test)]
            adopted_stop_gate::pass(&self.state).await;
            self.attempt_background_stop_close(&plan.record_id, &adopted_exec_session_id)
                .await;
        }
        if adopted_stop || matches!(reservation, BackgroundRunReservation::Reserved) {
            // An adopted stop changes the record's outcome, so observers need
            // it as much as a fresh claim.
            self.publish_background_status(&plan.record_id).await;
        }
        // The reservation has answered. From here the claim belongs to the
        // caller's launch, which arms its own guard with no await in between.
        claim_guard_disarm(claim_guard);
        Ok(reservation)
    }
}
