//! Plans a background launch, starts its exec session, and records its outcome.

use super::*;

use super::controller_background_query::resolve_background_target;
use super::controller_background_settlement::{
    clear_background_ledger_write_pending, mark_background_ledger_write_pending,
};

use std::sync::atomic::{AtomicU64, Ordering};

/// Returns the next number in this process's background-launch sequence.
///
/// Every launch consumes one number, so identifiers built from it are unique
/// even when two launches share a millisecond timestamp. The process id keeps
/// one process's sequence from colliding with another's.
fn next_background_launch_sequence() -> u64 {
    static LAUNCH_SEQUENCE: AtomicU64 = AtomicU64::new(0);
    LAUNCH_SEQUENCE.fetch_add(1, Ordering::Relaxed)
}

/// Builds a session id that no other launch in this process can repeat.
///
/// The base already carries a millisecond timestamp, but two launches of one
/// record in the same millisecond derived the same session id. The archive
/// attachment identifies a launch by its session id, so a repeat would let the
/// earlier run's archive paths attach to its replacement. A process-wide
/// sequence makes every launch distinct.
pub(super) fn background_launch_session_id(session_base: &str) -> String {
    format!("{session_base}-{:x}-{:x}", std::process::id(), next_background_launch_sequence())
}

/// Builds an exec-session id that no other launch in this process can repeat.
///
/// Two launches of one record in the same millisecond must not share a session
/// id, or the second spawn fails on the duplicate-session check while the
/// first is still starting — a failure settlement could blame on the winner's
/// live record. A process-wide counter makes every launch distinct.
pub(super) fn background_exec_session_id(session_id: &str) -> String {
    format!("exec-{session_id}-{:x}-{:x}", std::process::id(), next_background_launch_sequence())
}

struct BackgroundRunIdentity {
    spec: SubagentSpec,
    record_id: String,
    parent_session_id: String,
    session_id: String,
    exec_session_id: String,
    created_at: chrono::DateTime<Utc>,
    previous_record: Option<BackgroundRecord>,
}

impl SubagentController {
    pub(super) async fn background_status_for(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        let state = self.state.read().await;
        let record = resolve_background_target(&state, TaskOwner::Workspace, target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        Ok(record.build_status_entry())
    }

    /// Publishes the projection of one background record and returns it.
    ///
    /// The snapshot and the session that routes it are read under one state
    /// lock, so a concurrent restart that moves the record to another parent
    /// cannot deliver a stale projection to the new parent's observers.
    ///
    /// Ownership rule: a record whose id encodes its owner keeps that owner
    /// for life. `prepare_background_run` takes a record's owner from the
    /// record it replaces and never from a launch override, and an
    /// owner-scoped id cannot be re-created for a different session. A launch
    /// override therefore re-attaches the record — deliberately, because the
    /// caller names the session that now drives it — by changing its
    /// `parent_session_id`, not its owner.
    ///
    /// Routing follows that `parent_session_id`, not the owner.
    /// `observed_background_status` reads the record's parent session together
    /// with its projection under one state lock, and
    /// `publish_background_progress` delivers the snapshot to the observers
    /// registered for that parent. A snapshot therefore reaches the parent the
    /// record named when it was published, not whichever parent a later call
    /// happens to read.
    pub(super) async fn settle_background_status_and_publish(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        let record_id = {
            let state = self.state.read().await;
            resolve_background_target(&state, TaskOwner::Workspace, target)
                .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?
                .id
                .clone()
        };
        self.publish_background_progress(&record_id)
            .await
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))
    }

    pub(super) async fn ensure_background_record_running(
        &self,
        request: BackgroundRunRequest,
    ) -> Result<BackgroundSubprocessEntry> {
        let mut plan = self.prepare_background_run(request).await?;
        loop {
            match self.reserve_background_run(&plan).await? {
                BackgroundRunReservation::Reserved => {}
                // An active launch already runs this work; its projection is
                // the caller's answer, and a second process would duplicate it.
                BackgroundRunReservation::Reused(entry) => return Ok(*entry),
                // Another launch owns the record. It answers for itself, so the
                // caller receives what that launch has committed.
                BackgroundRunReservation::Declined => return self.background_status_for(&plan.record_id).await,
            }
            let metadata = match self.spawn_background_exec_session(&plan).await {
                Ok(metadata) => {
                    // The commit runs after the spawn returns, without the
                    // state lock, so a cancellation or replacement can land in
                    // that window. It is otherwise only reachable by racing a
                    // real process, so a test can hold it here.
                    #[cfg(test)]
                    pass_background_commit_gate(&self.state).await;
                    metadata
                }
                Err(error) => {
                    self.settle_failed_background_run(&plan, &error).await;
                    // A spawn failure is the only thing that spends the
                    // restart allowance, and `settle_failed_background_run`
                    // leaves the record `Starting` exactly when the allowance
                    // remains. Nothing else observes that state as a launch to
                    // drive, so the retry has to happen here; otherwise the
                    // record stays `Starting` forever and is reported as an
                    // active process that will never run.
                    let Some(retry) = self.background_launch_retry_request(&plan).await else {
                        return Err(error);
                    };
                    tracing::warn!(
                        agent_name = retry.agent_name.as_str(),
                        record_id = plan.record_id.as_str(),
                        attempt = retry.restart_attempts,
                        error = %error,
                        "Background launch failed; retrying once"
                    );
                    plan = match self.prepare_background_run(retry).await {
                        Ok(prepared) => prepared,
                        Err(prepare_error) => {
                            // Nothing else drives a record once its replacement
                            // cannot even be prepared, so settle it rather than
                            // report a launch that will never run.
                            self.settle_failed_background_run(&plan, &prepare_error).await;
                            return Err(prepare_error);
                        }
                    };
                    continue;
                }
            };
            let Some(write_generation) = self.record_background_run_started(&plan, metadata).await? else {
                // The record stopped wanting this launch while the spawn was in
                // flight. The session exists now and nothing records it, so
                // close it rather than leave an untracked process running. A
                // failed close stays owed under the launch's own exec session:
                // the record names another launch (or none), so a close keyed
                // to the record's current session could never be retried.
                self.close_declined_background_session(&plan.record_id, &plan.exec_session_id)
                    .await;
                return self.background_status_for(&plan.record_id).await;
            };
            // The record is committed in memory but the ledger is not. The
            // commit raised the owed-write obligation under the lock that made
            // the record reusable, so no concurrent request can treat it as
            // durable before the save is attempted. `?` leaves the obligation
            // owed when the save fails; only a successful write clears the
            // generation this call observed.
            self.save_background_state().await?;
            clear_background_ledger_write_pending(&self.state, write_generation);
            return self.background_status_for(&plan.record_id).await;
        }
    }

    /// Builds the single retry a failed launch is still allowed, if any.
    ///
    /// The allowance is spent here, under the write lock, so a concurrent
    /// refresh cannot schedule the same replacement from an unchanged record.
    /// The retry is offered only while the failed launch still owns the record:
    /// once `settle_failed_background_run` has settled it, or a cancellation
    /// has cleared `desired_enabled`, the failure is terminal and a retry would
    /// undo that outcome.
    async fn background_launch_retry_request(&self, plan: &BackgroundRunPlan) -> Option<BackgroundRunRequest> {
        if !self.config.vt_cfg.subagents.background.auto_restore {
            return None;
        }
        let next_restart_attempt = {
            let mut state = self.state.write().await;
            let record = state.background_children.get_mut(&plan.record_id)?;
            if !failed_launch_still_owns_record(record, plan.exec_session_id.as_str()) || record.restart_attempts >= 1 {
                return None;
            }
            record.restart_attempts = record.restart_attempts.saturating_add(1);
            record.updated_at = Utc::now();
            record.restart_attempts
        };
        Some(BackgroundRunRequest {
            agent_name: plan.spec.name.as_str().to_string(),
            stable_id: Some(plan.record_id.clone()),
            restart_attempts: next_restart_attempt,
            overrides: None,
            // The retry replaces the launch that just failed. Binding it to
            // that exec session stops a late retry from overwriting a newer
            // launch that has since taken the record.
            expected_exec_session_id: Some(plan.exec_session_id.clone()),
        })
    }

    /// Resolves the spec, record and session identity for one launch request.
    ///
    /// A parent-session override is accepted: when the request names one, that
    /// session becomes the launch's `parent_session_id`. An existing record
    /// keeps its `owner_session_id` for life regardless.
    /// `prepare_background_run` takes the owner from the record being
    /// replaced, never from the override, and an owner-encoding id cannot
    /// name a different owner anyway. Observers therefore route the new
    /// snapshot to the override session while the record still belongs to its
    /// original owner.
    async fn resolve_background_run_identity(&self, request: &BackgroundRunRequest) -> Result<BackgroundRunIdentity> {
        let spec = self
            .resolve_requested_spec(Some(request.agent_name.as_str()))
            .await
            .with_context(|| format!("Failed to resolve background subagent '{}'", request.agent_name))?;
        let requested_record_id = request.stable_id.clone().unwrap_or_else(|| {
            background_record_id_for_owner(request.agent_name.as_str(), self.background_owner_session_id.as_deref())
        });
        // Every in-tree caller passes a canonical id: the launch paths derive
        // it from the agent and owner, and restore paths take it from the map
        // key. The legacy alias fallback in `resolve_background_target` exists
        // for client lookups; applying it here let an unscoped controller
        // resolve another owner's `agent@session` record and then rewrite it
        // under this controller's view. An exact lookup keeps a launch bound
        // to the record its caller named.
        let previous_record = {
            let state = self.state.read().await;
            state.background_children.get(&requested_record_id).cloned()
        };
        let record_id = previous_record.as_ref().map_or(requested_record_id, |record| record.id.clone());
        let previous_parent_session_id = usable_background_parent_session_id(
            previous_record.as_ref().map(|record| record.parent_session_id.clone()),
        );
        let current_parent_session_id = self.parent_session_id.read().await.clone();
        let requested_parent_session_id = usable_background_parent_session_id(
            request
                .overrides
                .as_ref()
                .and_then(|overrides| overrides.parent_session_id.clone()),
        );
        let parent_session_id = requested_parent_session_id
            .or(previous_parent_session_id)
            .unwrap_or(current_parent_session_id);
        let session_base = format!(
            "{}-{}-{}",
            sanitize_component(parent_session_id.as_str()),
            sanitize_component(record_id.as_str()),
            Utc::now().format("%Y%m%dT%H%M%S%3fZ")
        );
        let session_id = background_launch_session_id(&session_base);
        let exec_session_id = background_exec_session_id(&session_id);
        let created_at = previous_record.as_ref().map_or_else(Utc::now, |record| record.created_at);
        Ok(BackgroundRunIdentity {
            spec,
            record_id,
            parent_session_id,
            session_id,
            exec_session_id,
            created_at,
            previous_record,
        })
    }

    async fn prepare_background_run(&self, request: BackgroundRunRequest) -> Result<BackgroundRunPlan> {
        // A replacement preparation is fallible only through spec resolution,
        // and the discovered spec snapshot is fixed at construction. A test
        // cannot otherwise make the first preparation succeed and the retry's
        // fail, so the seam drops the bound attempt's spec before it resolves.
        #[cfg(test)]
        if request.expected_exec_session_id.is_some() && take_background_retry_spec_removal(&self.state) {
            self.state
                .write()
                .await
                .discovered
                .effective
                .retain(|spec| !spec.matches_name(request.agent_name.as_str()));
        }
        let identity = self.resolve_background_run_identity(&request).await?;
        let previous_record = identity.previous_record.as_ref();
        let previous_prompt = previous_record.map_or_else(String::new, |record| record.prompt.clone());
        let prompt = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.prompt.clone())
            .filter(|value| !value.trim().is_empty())
            .or_else(|| (!previous_prompt.trim().is_empty()).then_some(previous_prompt))
            .or_else(|| identity.spec.initial_prompt.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "You are the Diapason background subagent `{}`. Summarize readiness briefly, inspect the workspace at a high level, then remain idle until the process is terminated.",
                    identity.spec.name
                )
            });
        let max_turns = normalize_background_child_max_turns(
            request
                .overrides
                .as_ref()
                .and_then(|overrides| overrides.max_turns)
                .or_else(|| previous_record.and_then(|record| record.max_turns))
                .or(identity.spec.max_turns),
            true,
        );
        let model_override = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.model_override.clone())
            .or_else(|| previous_record.and_then(|record| record.model_override.clone()))
            .or_else(|| identity.spec.model.clone());
        let reasoning_override = request
            .overrides
            .as_ref()
            .and_then(|overrides| overrides.reasoning_override.clone())
            .or_else(|| previous_record.and_then(|record| record.reasoning_override.clone()))
            .or_else(|| {
                identity
                    .spec
                    .reasoning_effort
                    .as_ref()
                    .map(|effort| effort.as_str().to_string())
            });

        // An owner-scoped controller tags its own records so sibling sessions
        // in one workspace cannot adopt or overwrite them. An unscoped
        // controller manages the whole workspace ledger, so the records it
        // creates are workspace-global and ownerless; giving them a per-run
        // owner would change their canonical id on every restart.
        let owner_session_id = match previous_record {
            Some(record) => record.owner_session_id.clone(),
            None => self.background_owner_session_id.clone(),
        };
        Ok(BackgroundRunPlan {
            owner_session_id,
            spec: identity.spec,
            record_id: identity.record_id,
            parent_session_id: identity.parent_session_id,
            session_id: identity.session_id,
            exec_session_id: identity.exec_session_id,
            created_at: identity.created_at,
            prompt,
            max_turns,
            model_override,
            reasoning_override,
            restart_attempts: request.restart_attempts,
            expected_exec_session_id: request.expected_exec_session_id,
        })
    }

    async fn spawn_background_exec_session(
        &self,
        plan: &BackgroundRunPlan,
    ) -> Result<crate::tools::types::DiapasonExecSession> {
        let launch = build_background_launch_spec(
            &self.config.workspace_root,
            plan.spec.name.as_str(),
            plan.parent_session_id.as_str(),
            plan.session_id.as_str(),
            plan.prompt.as_str(),
            plan.max_turns,
            plan.model_override.as_deref(),
            plan.reasoning_override.as_deref(),
        )?;
        let metadata = if launch.use_pty {
            self.config
                .exec_sessions
                .create_pty_session(
                    plan.exec_session_id.clone().into(),
                    launch.command,
                    self.config.workspace_root.clone(),
                    PtySize {
                        rows: 24,
                        cols: 80,
                        pixel_width: 0,
                        pixel_height: 0,
                    },
                    hashbrown::HashMap::new(),
                    None,
                )
                .await
        } else {
            self.config
                .exec_sessions
                .create_pipe_session(
                    plan.exec_session_id.clone().into(),
                    launch.command,
                    self.config.workspace_root.clone(),
                    hashbrown::HashMap::new(),
                )
                .await
        }
        .with_context(|| format!("Failed to spawn background subprocess for subagent '{}'", plan.spec.name))?;

        tracing::info!(
            agent_name = plan.spec.name.as_str(),
            record_id = plan.record_id.as_str(),
            exec_session_id = plan.exec_session_id.as_str(),
            pid = metadata.child_pid,
            "Spawned background subagent subprocess"
        );
        Ok(metadata)
    }

    /// Commits a spawned launch to its record, unless that record no longer wants it.
    ///
    /// The spawn runs without the state lock, so any controller action can
    /// change the record while it is in flight: a cancellation can clear
    /// `desired_enabled`, a refresh can schedule a replacement, and a stop
    /// another controller persisted can be adopted. Writing this launch over
    /// that change would resurrect a cancelled task, attach one process's
    /// metadata to a different launch, or keep a process alive that no record
    /// names. The commit re-checks the record under the write lock and reports
    /// `false` when it declined, leaving the record as the concurrent action
    /// left it; the caller closes the session it created. A launch also refuses
    /// an exec-session id another record holds: sharing one id would make the
    /// two indistinguishable to pruning and cancellation, so the launch is
    /// declined and the duplicate session closed.
    async fn record_background_run_started(
        &self,
        plan: &BackgroundRunPlan,
        metadata: crate::tools::types::DiapasonExecSession,
    ) -> Result<Option<u64>> {
        self.commit_background_run_started(
            &plan.record_id,
            &plan.exec_session_id,
            metadata.child_pid,
            metadata.started_at,
        )
        .await
    }
}

/// A test gate between a successful spawn and the commit that records it.
///
/// The commit runs without the state lock, so a cancellation or replacement can
/// land between the spawn and the record write. No real process schedules that
/// race deterministically, so a test arms this gate to land the change and then
/// observe the close the declined commit owes.
#[cfg(test)]
struct BackgroundCommitGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
fn background_commit_gates() -> &'static Mutex<Vec<BackgroundCommitGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<BackgroundCommitGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next launch of `state` before its commit.
///
/// Returns the signal that the launch reached the window and the signal that
/// releases it, so the test can land a concurrent change in between.
#[cfg(all(test, unix))]
pub(super) fn gate_next_background_commit(state: &Arc<RwLock<ControllerState>>) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = background_commit_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(BackgroundCommitGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

#[cfg(test)]
async fn pass_background_commit_gate(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = background_commit_gates()
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

/// Marks that the next bound (replacement) preparation of `state` must fail.
///
/// The seam only affects a request carrying `expected_exec_session_id`, because
/// that is the retry the inline launch path prepares after a spent allowance;
/// the first, unbound preparation still resolves as usual.
#[cfg(test)]
fn pending_background_retry_spec_removals() -> &'static Mutex<Vec<Weak<RwLock<ControllerState>>>> {
    static PENDING: std::sync::OnceLock<Mutex<Vec<Weak<RwLock<ControllerState>>>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

#[cfg(all(test, unix))]
pub(super) fn remove_spec_before_next_background_retry(state: &Arc<RwLock<ControllerState>>) {
    let mut pending = pending_background_retry_spec_removals()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending.retain(|entry| entry.strong_count() > 0);
    pending.push(Arc::downgrade(state));
}

#[cfg(test)]
fn take_background_retry_spec_removal(state: &Arc<RwLock<ControllerState>>) -> bool {
    let mut pending = pending_background_retry_spec_removals()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    pending.retain(|entry| entry.strong_count() > 0);
    let Some(index) = pending
        .iter()
        .position(|entry| std::ptr::eq(entry.as_ptr(), Arc::as_ptr(state)))
    else {
        return false;
    };
    pending.remove(index);
    true
}
