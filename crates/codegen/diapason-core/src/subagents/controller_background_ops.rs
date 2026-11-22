//! Manages background subagent work, including task completion, cancellation, and archived results.

mod controller_background_archive;
mod controller_background_cancel;
mod controller_background_launch;
mod controller_background_lifecycle;
mod controller_background_liveness;
mod controller_background_query;
mod controller_background_run;
mod controller_background_settlement;

#[cfg(test)]
mod tests;

use super::*;

pub use self::controller_background_cancel::BackgroundStopOutcome;
use self::controller_background_cancel::background_stop_close_is_pending;
pub(in crate::subagents) use self::controller_background_cancel::rekey_pending_background_stop_close;
use self::controller_background_lifecycle::{clear_background_launch_owed, mark_background_launch_owed};
use self::controller_background_liveness::exec_session_is_missing;
use self::controller_background_query::{read_output_tail, read_preview};
use self::controller_background_settlement::{pending_background_ledger_write_generation, restart_allowed};

pub(super) struct BackgroundRunRequest {
    pub(super) agent_name: String,
    pub(super) stable_id: Option<String>,
    pub(super) restart_attempts: u8,
    pub(super) overrides: Option<BackgroundLaunchOverrides>,
    /// The launch this request replaces, when it restarts an observed one.
    ///
    /// Refresh decides to restart from an observation made outside the state
    /// lock, so by the time it runs the record may already name a newer
    /// launch. The expected exec session binds the replacement to the launch
    /// it examined: the reservation declines when the record names another
    /// session, instead of overwriting the newer launch. An explicit toggle or
    /// spawn passes `None` because it replaces no particular observed launch.
    pub(super) expected_exec_session_id: Option<String>,
}

/// The identity one background launch was prepared with.
///
/// `pub(super)` rather than private so the controller's tests can drive the
/// settlement path with a launch that has already been replaced, which is the
/// only way to exercise the ownership guard without racing a real spawn.
pub(super) struct BackgroundRunPlan {
    pub(super) owner_session_id: Option<String>,
    pub(super) spec: SubagentSpec,
    pub(super) record_id: String,
    pub(super) parent_session_id: String,
    pub(super) session_id: String,
    pub(super) exec_session_id: String,
    pub(super) created_at: chrono::DateTime<Utc>,
    pub(super) prompt: String,
    pub(super) max_turns: Option<usize>,
    pub(super) model_override: Option<String>,
    pub(super) reasoning_override: Option<String>,
    pub(super) restart_attempts: u8,
    /// The launch this plan may replace, carrying the request's expectation.
    pub(super) expected_exec_session_id: Option<String>,
}

/// How the reservation step answered one prepared launch.
#[derive(Debug)]
enum BackgroundRunReservation {
    /// The plan now owns the record at `Starting`, so it may spawn.
    Reserved,
    /// An active launch already satisfies the plan and is returned as it is.
    ///
    /// Boxed so the reservation stays small: a snapshot entry is an order of
    /// magnitude larger than the other variants, and every `Reserved` or
    /// `Declined` answer would otherwise carry that size.
    Reused(Box<BackgroundSubprocessEntry>),
    /// A different launch owns the record, which was left untouched.
    Declined,
}

fn usable_background_parent_session_id(owner: Option<String>) -> Option<String> {
    owner.filter(|owner| !owner.is_empty())
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
fn failed_launch_still_owns_record(record: &BackgroundRecord, plan_exec_session_id: &str) -> bool {
    record.exec_session_id == plan_exec_session_id
        && matches!(record.status, BackgroundSubprocessStatus::Starting)
        && record.desired_enabled
}

/// Names the launch settings a record and a desired launch disagree on.
///
/// Reuse is only safe when the record already runs the work the caller asked
/// for, because the launch settings are the identity of that work. A caller
/// that wants a different prompt or model must stop the existing process
/// first: replacing a live launch in place would orphan the process already
/// running.
pub(super) fn background_launch_conflicts(
    record: &BackgroundRecord,
    prompt: &str,
    max_turns: Option<usize>,
    model_override: Option<&str>,
    reasoning_override: Option<&str>,
) -> Vec<&'static str> {
    let mut conflicts = Vec::new();
    if record.prompt != prompt {
        conflicts.push("prompt");
    }
    if record.max_turns != max_turns {
        conflicts.push("max_turns");
    }
    if record.model_override.as_deref() != model_override {
        conflicts.push("model");
    }
    if record.reasoning_override.as_deref() != reasoning_override {
        conflicts.push("reasoning_effort");
    }
    conflicts
}

/// Projects a prepared plan onto the record the plan now owns.
fn starting_background_record(plan: &BackgroundRunPlan) -> BackgroundRecord {
    BackgroundRecord {
        owner_session_id: plan.owner_session_id.clone(),
        id: plan.record_id.clone(),
        agent_name: plan.spec.name.clone(),
        display_label: subagent_display_label(&plan.spec),
        description: plan.spec.description.clone(),
        source: plan.spec.source.label(),
        colour: plan.spec.colour.clone(),
        session_id: plan.session_id.clone(),
        parent_session_id: plan.parent_session_id.clone(),
        exec_session_id: plan.exec_session_id.clone(),
        desired_enabled: true,
        status: BackgroundSubprocessStatus::Starting,
        created_at: plan.created_at,
        updated_at: Utc::now(),
        started_at: None,
        ended_at: None,
        pid: None,
        prompt: plan.prompt.clone(),
        summary: Some("Starting background subagent".to_string()),
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: plan.max_turns,
        model_override: plan.model_override.clone(),
        reasoning_override: plan.reasoning_override.clone(),
        restart_attempts: plan.restart_attempts,
    }
}

impl SubagentController {
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
    pub(super) async fn settle_failed_background_run(&self, plan: &BackgroundRunPlan, error: &anyhow::Error) {
        self.settle_background_launch_error(&plan.record_id, &plan.exec_session_id, error, true)
            .await;
    }

    /// Settles a record whose scheduled replacement could not be prepared.
    ///
    /// The restart allowance was already spent when refresh scheduled the
    /// replacement, so a preparation failure cannot be retried. Leaving the
    /// record `Starting` would report an active launch that nothing will ever
    /// drive, and refresh exempts a `Starting` record only while it still owes
    /// a launch, so the failure is settled here and its obligation discharged.
    /// The observation's exec session still identifies the launch, so only a
    /// record that still names it is settled.
    pub(super) async fn settle_failed_background_restart(
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
        {
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
            record.error = Some(format!("Failed to start background subagent '{}': {error:#}", record.agent_name));
            record.summary = Some("Background subagent failed to start".to_string());
            record.updated_at = Utc::now();
            record.ended_at.get_or_insert(Utc::now());
        }
        self.publish_background_status(record_id).await;
        // The caller receives the original spawn error, so a persistence
        // failure must not replace it; the ledger catches up on the next save.
        if let Err(save_error) = self.save_background_state().await {
            tracing::warn!(
                record_id,
                error = %save_error,
                "Failed to persist the settled background launch failure"
            );
        }
    }

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
        exec_session_id: &str,
    ) -> Result<(Option<PersistedBackgroundRecord>, bool)> {
        if self.config.managed_background_runtime {
            return Ok((None, false));
        }
        let mut persisted = controller_persistence::collect_by_canonical_id(
            load_background_state(&self.config.workspace_root).await?.records,
        );
        let moved = persisted
            .iter()
            .any(|(id, record)| id != record_id && record.exec_session_id == exec_session_id);
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
    async fn reserve_background_run(&self, plan: &BackgroundRunPlan) -> Result<BackgroundRunReservation> {
        // A record whose ledger write is still owed must not be answered from
        // memory. Reuse would report a `Running` record that a restart or
        // another process may never see. Retrying here makes the reservation
        // either wait for that write or fail visibly; it can never return
        // `Reused` while the write is outstanding.
        if pending_background_ledger_write_generation(&self.state).is_some() {
            self.retry_owed_background_ledger_write().await?;
        }
        let ledger_guard = if plan.expected_exec_session_id.is_some() && !self.config.managed_background_runtime {
            // The file lock is taken before the state lock and released before
            // anything publishes, matching `save_background_state`'s order.
            Some(lock_background_state(&self.config.workspace_root).await?)
        } else {
            None
        };
        let (persisted, launch_moved) = if ledger_guard.is_some() {
            self.persisted_record_for(&plan.record_id, plan.exec_session_id.as_str())
                .await?
        } else {
            (None, false)
        };
        let (reservation, adopted_exec_session_id) = {
            let mut state = self.state.write().await;
            if let Some(expected_exec_session_id) = plan.expected_exec_session_id.as_deref() {
                let still_owns_launch = state
                    .background_children
                    .get(&plan.record_id)
                    .is_some_and(|record| record.exec_session_id == expected_exec_session_id && record.desired_enabled);
                // The launch this plan observed is no longer here under this
                // id, or it was migrated to its owner and is now persisted
                // elsewhere. Either way it is not this controller's to
                // replace: spawning now would put a second process beside the
                // migrated record.
                if !still_owns_launch || launch_moved {
                    (BackgroundRunReservation::Declined, None)
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
                        (BackgroundRunReservation::Declined, Some(exec_session_id))
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
                        (BackgroundRunReservation::Reserved, None)
                    }
                }
            } else if state.background_children.get(&plan.record_id).is_some_and(|record| {
                background_stop_close_is_pending(&self.state, &plan.record_id, &record.exec_session_id)
            }) {
                // A stop is not finished until its process is closed. Replacing
                // the record now would publish a new launch over a process no
                // record names, and clear the obligation that a later reconcile
                // still needs to discharge.
                (BackgroundRunReservation::Declined, None)
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
                        (conflicts, record.build_status_entry())
                    })
                });
                let reservation = match active {
                    Some((conflicts, entry)) if conflicts.is_empty() => {
                        BackgroundRunReservation::Reused(Box::new(entry))
                    }
                    Some((conflicts, _)) => {
                        bail!(
                            "spawn_background_subprocess found active background subprocess '{}' with different {}. Stop or restart the existing subprocess before changing its launch settings.",
                            plan.spec.name,
                            conflicts.join(", "),
                        )
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
                        BackgroundRunReservation::Reserved
                    }
                };
                (reservation, None)
            }
        };
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
            pass_adopted_background_stop_close_gate(&self.state).await;
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
}

/// A test gate between adopting a persisted stop and closing its launch.
///
/// The adopted launch's id is captured under the state lock, but the close runs
/// after that lock (and the ledger lock) is released. A concurrent start can
/// take the record in that window, and no real scheduling makes the race
/// deterministic, so a test arms this gate to land the replacement start and
/// then observe which session the close addresses.
#[cfg(test)]
struct AdoptedStopCloseGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
fn adopted_stop_close_gates() -> &'static Mutex<Vec<AdoptedStopCloseGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<AdoptedStopCloseGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next adopted-stop close of `state`.
#[cfg(test)]
fn gate_next_adopted_background_stop_close(state: &Arc<RwLock<ControllerState>>) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = adopted_stop_close_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(AdoptedStopCloseGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

#[cfg(test)]
async fn pass_adopted_background_stop_close_gate(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = adopted_stop_close_gates()
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

#[cfg(test)]
mod reserve_tests {
    //! Pins the adopted-stop close to the launch whose stop was adopted.

    use std::path::PathBuf;
    use std::time::Duration;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::subagents::background::persist_background_state;
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::controller_background_cancel::gate_next_background_close;
    use super::*;

    const WORKER: &str = "background-worker";
    const WORKER_AGENT: &str = "worker";
    const OBSERVED_EXEC: &str = "exec-observed";

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

    fn background_record(desired_enabled: bool) -> BackgroundRecord {
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
            exec_session_id: OBSERVED_EXEC.to_string(),
            desired_enabled,
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

    fn persisted_stop() -> PersistedBackgroundRecord {
        serde_json::from_value(serde_json::json!({
            "id": WORKER,
            "owner_session_id": null,
            "agent_name": WORKER_AGENT,
            "display_label": WORKER_AGENT,
            "description": WORKER_AGENT,
            "source": "builtin",
            "color": null,
            "session_id": format!("session-{WORKER}"),
            "exec_session_id": OBSERVED_EXEC,
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

    fn plan(spec: SubagentSpec, exec_session_id: &str, expected: Option<&str>) -> BackgroundRunPlan {
        BackgroundRunPlan {
            owner_session_id: None,
            spec,
            record_id: WORKER.to_string(),
            parent_session_id: "parent-session".to_string(),
            session_id: format!("session-{exec_session_id}"),
            exec_session_id: exec_session_id.to_string(),
            created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
            prompt: "work".to_string(),
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
            expected_exec_session_id: expected.map(str::to_owned),
        }
    }

    /// An adopted stop closes the launch it adopted, not a replacement.
    ///
    /// The stop is adopted under the state lock, but the close runs after that
    /// lock is released. A concurrent explicit start can take the record in
    /// between; re-reading the record then would close the replacement's
    /// session and settle a launch that never owed the stop. The gate lands
    /// the concurrent start in that window.
    #[tokio::test]
    async fn an_adopted_stop_closes_the_launch_it_adopted_after_a_concurrent_start() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), background_record(true));
        }
        persist_background_state(temp.path(), vec![persisted_stop()])
            .await
            .expect("seed the persisted stop");

        let spec = diapason_config::builtin_subagents()
            .into_iter()
            .find(|spec| spec.name == WORKER_AGENT)
            .expect("worker spec");
        let bound_plan = plan(spec.clone(), OBSERVED_EXEC, Some(OBSERVED_EXEC));
        let replacement_plan = plan(spec, "exec-replacement", None);

        let (close_entered, close_release) = gate_next_background_close(&controller.state, OBSERVED_EXEC);
        let (reservation_entered, reservation_release) = gate_next_adopted_background_stop_close(&controller.state);
        let reserve = controller.reserve_background_run(&bound_plan);
        tokio::pin!(reserve);
        tokio::select! {
            result = &mut reserve => {
                panic!("the adopted stop closed before the concurrent start landed: {result:?}")
            },
            () = reservation_entered.notified() => {}
        }

        // A concurrent explicit start takes the record now that the adopted
        // stop has cleared its intent, and names a different exec session.
        let concurrent = controller
            .reserve_background_run(&replacement_plan)
            .await
            .expect("the replacement launch reserves the record");
        assert!(matches!(concurrent, BackgroundRunReservation::Reserved));

        reservation_release.notify_one();
        // The reservation future is only polled while something awaits it, so
        // drive it here: the close gate can only be reached from inside that
        // future, and waiting on the gate alone would wait on a future nobody
        // polls.
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                () = close_entered.notified() => {}
                result = &mut reserve => {
                    panic!("the adopted stop answered before it closed its launch: {result:?}")
                }
            }
        })
        .await
        .expect("the adopted stop must close the launch whose stop it adopted");
        close_release.notify_one();

        let reservation = reserve.await.expect("the adopted stop still answers the caller");
        assert!(matches!(reservation, BackgroundRunReservation::Declined));

        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(record.exec_session_id, "exec-replacement", "the replacement launch keeps the record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "the adopted stop's close must not settle the replacement launch"
        );
        assert!(record.desired_enabled, "the replacement launch is still wanted");
        drop(state);

        controller.signal_shutdown().await;
    }
}
