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

use std::collections::BTreeSet;

use super::controller_persistence::persist_background_records;

pub use self::controller_background_cancel::BackgroundStopOutcome;
use self::controller_background_cancel::background_stop_close_is_pending;
pub(in crate::subagents) use self::controller_background_cancel::rekey_pending_background_stop_close;
use self::controller_background_lifecycle::mark_background_launch_owed;
use self::controller_background_liveness::exec_session_is_missing;
use self::controller_background_query::{read_output_tail, read_preview};
use self::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
    persisted_background_row_is_desired,
};

pub(in crate::subagents) use self::reservation::{BackgroundRunPlan, BackgroundRunRequest, BackgroundRunReservation};
use self::reservation::{background_launch_conflicts, starting_background_record, usable_background_parent_session_id};

impl SubagentController {
    /// Reads one record's persisted copy and whether the same launch moved.
    ///
    /// The caller holds the ledger's file lock, so neither answer can change
    /// between this read and the state write that acts on it. Every save takes
    /// that lock, in this process and in any other that shares the workspace.
    ///
    /// A launch whose row was migrated to its owner is persisted under that
    /// owner's canonical id, so a bound plan still addressing the ownerless id
    /// finds no copy here. The launch has not disappeared, it has moved, and
    /// reading that separately is what stops the reservation from replacing a
    /// migrated record with a second process beside it.
    async fn persisted_record_for(
        &self,
        record_id: &str,
        observed_exec_session_id: &str,
    ) -> Result<(Option<PersistedBackgroundRecord>, bool)> {
        if self.config.managed_background_runtime {
            return Ok((None, false));
        }
        let mut persisted = controller_persistence::collect_by_canonical_id(
            load_background_state(&self.config.workspace_root).await?.records,
        );
        // A row under another canonical id naming the observed launch means the
        // launch was migrated to its owner. Its intent does not matter: a
        // stopped migration is still not this controller's to replace.
        let moved = persisted
            .iter()
            .any(|(id, record)| id != record_id && record.exec_session_id == observed_exec_session_id);
        Ok((persisted.remove(record_id), moved))
    }

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
    /// fact both processes must agree on.
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
        } else if plan.expected_exec_session_id.is_some() {
            // A bound replacement must be one durable transition with the stop
            // it reconciles, so a contended or unreadable ledger fails it.
            Some(lock_background_state(&self.config.workspace_root).await?)
        } else {
            // An explicit start replaces no observed launch, so an unavailable
            // ledger cannot make it unsafe; it falls back to the ordinary save,
            // which retries the owed write, exactly as before.
            lock_background_state(&self.config.workspace_root).await.ok()
        };
        let (persisted, launch_moved) = if plan.expected_exec_session_id.is_some() && ledger_guard.is_some() {
            self.persisted_record_for(&plan.record_id, plan.observed_exec_session_id())
                .await?
        } else {
            (None, false)
        };
        let (reservation, adopted_exec_session_id, claim_is_durable) = {
            let mut state = self.state.write().await;
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
                // migrated record.
                if !still_owns_launch || launch_moved || competing_claim {
                    (BackgroundRunReservation::Declined, None, None)
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
                        (BackgroundRunReservation::Declined, Some(exec_session_id), None)
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
                        (BackgroundRunReservation::Reserved, None, Some(ledger_guard.is_some()))
                    }
                }
            } else if state.background_children.get(&plan.record_id).is_some_and(|record| {
                background_stop_close_is_pending(&self.state, &plan.record_id, &record.exec_session_id)
            }) {
                // A stop is not finished until its process is closed. Replacing
                // the record now would publish a new launch over a process no
                // record names, and clear the obligation that a later reconcile
                // still needs to discharge.
                (BackgroundRunReservation::Declined, None, None)
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
                let (reservation, claim_is_durable) = match active {
                    Some((conflicts, entry, parent_session_id)) => {
                        // The reuse check refuses a record owned by another
                        // invocation's parent before the state lock is taken.
                        // The same rule applies here, where the decision is
                        // really made: answering for another parent's launch
                        // would report a task this invocation does not drive.
                        if current_subagent_invocation_context()
                            .is_some_and(|context| context.parent_session_id != parent_session_id)
                        {
                            bail!("A background subprocess with this agent name is already active");
                        }
                        if conflicts.is_empty() {
                            (BackgroundRunReservation::Reused(Box::new(entry)), None)
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
                        (BackgroundRunReservation::Reserved, Some(ledger_guard.is_some()))
                    }
                };
                (reservation, None, claim_is_durable)
            }
        };
        // The claim is persisted while the ledger lock is still held, so another
        // process cannot read the previous row and reserve its own replacement
        // in the window between this decision and its write. The ordinary save
        // would re-lock the ledger, so the row is written directly here.
        if claim_is_durable == Some(true)
            && let Err(error) = self
                .persist_reserved_background_claim(&plan.record_id, &plan.exec_session_id)
                .await
        {
            // A bound replacement must not spawn a process the ledger cannot
            // name: another process would read the previous row and start its
            // own. The claim is withdrawn so the caller reports the fault
            // instead of publishing a launch that never ran.
            if plan.expected_exec_session_id.is_some() {
                // The settlement saves the record, so the ledger lock must be
                // released first: the save takes the same lock.
                drop(ledger_guard);
                self.settle_failed_background_restart(&plan.record_id, &plan.exec_session_id, &error)
                    .await;
                return Err(error);
            }
            // An explicit start keeps the process it is about to spawn and owes
            // the write, exactly as a failed save has always been handled.
            mark_background_ledger_write_pending(&self.state);
            tracing::warn!(
                record_id = plan.record_id.as_str(),
                error = %error,
                "Failed to persist the background launch claim; the write remains owed"
            );
        } else if claim_is_durable == Some(false) && !self.config.managed_background_runtime {
            // The ledger was unavailable, so the claim exists only in memory.
            // It owes the write from this moment, so reuse cannot answer from a
            // record no other process could see.
            mark_background_ledger_write_pending(&self.state);
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
        Ok(reservation)
    }

    /// Writes the record now claiming one launch into the ledger.
    ///
    /// The caller holds the ledger's file lock, so this cannot go through
    /// [`Self::save_background_state`], which takes the same lock in its
    /// persistence task. The record is re-read under the state lock first: a
    /// cancellation can land between the reservation and this write, and
    /// writing the claim it observed would resurrect a stopped task in the
    /// ledger. A record that has moved to another launch writes nothing, and
    /// the caller's later save carries whatever it became.
    async fn persist_reserved_background_claim(&self, record_id: &str, exec_session_id: &str) -> Result<()> {
        let claimed = {
            let state = self.state.read().await;
            let Some(record) = state.background_children.get(record_id) else {
                return Ok(());
            };
            if record.exec_session_id != exec_session_id {
                return Ok(());
            }
            record.clone()
        };
        let mut persisted = claimed.into_persisted();
        persisted.canonicalize_id();
        persist_background_records(
            &self.config.workspace_root,
            self.background_owner_session_id.as_deref(),
            vec![persisted],
            &BTreeSet::new(),
            &BTreeMap::new(),
        )
        .await
        .with_context(|| format!("Failed to persist the background claim for {record_id}"))
    }
}
