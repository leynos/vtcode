//! The launch identity a reservation claims and the answer it gives.

use super::*;

use std::collections::{BTreeMap, BTreeSet};

use super::controller_background_lifecycle::clear_background_launch_owed;
use super::controller_background_settlement::{
    mark_background_ledger_write_pending, persisted_background_row_is_desired,
};

pub(in crate::subagents) struct BackgroundRunRequest {
    pub(in crate::subagents) agent_name: String,
    pub(in crate::subagents) stable_id: Option<String>,
    pub(in crate::subagents) restart_attempts: u8,
    pub(in crate::subagents) overrides: Option<BackgroundLaunchOverrides>,
    /// The launch this request replaces, when it restarts an observed one.
    ///
    /// Refresh decides to restart from an observation made outside the state
    /// lock, so by the time it runs the record may already name a newer
    /// launch. The expected exec session binds the replacement to the launch
    /// it examined: the reservation declines when the record names another
    /// session, instead of overwriting the newer launch. An explicit toggle or
    /// spawn passes `None` because it replaces no particular observed launch.
    pub(in crate::subagents) expected_exec_session_id: Option<String>,
}

/// The identity one background launch was prepared with.
///
/// `pub(super)` rather than private so the controller's tests can drive the
/// settlement path with a launch that has already been replaced, which is the
/// only way to exercise the ownership guard without racing a real spawn.
pub(in crate::subagents) struct BackgroundRunPlan {
    pub(in crate::subagents) owner_session_id: Option<String>,
    pub(in crate::subagents) spec: SubagentSpec,
    pub(in crate::subagents) record_id: String,
    pub(in crate::subagents) parent_session_id: String,
    pub(in crate::subagents) session_id: String,
    pub(in crate::subagents) exec_session_id: String,
    pub(in crate::subagents) created_at: chrono::DateTime<Utc>,
    pub(in crate::subagents) prompt: String,
    pub(in crate::subagents) max_turns: Option<usize>,
    pub(in crate::subagents) model_override: Option<String>,
    pub(in crate::subagents) reasoning_override: Option<String>,
    pub(in crate::subagents) restart_attempts: u8,
    /// The launch this plan may replace, carrying the request's expectation.
    pub(in crate::subagents) expected_exec_session_id: Option<String>,
}

impl BackgroundRunPlan {
    /// The exec session this plan's reservation must recognize as the launch
    /// it observed.
    ///
    /// A bound plan replaces the launch its observation named, so the ledger is
    /// scanned for that session. A migrated row is persisted under its owner's
    /// canonical id, and the launch that replaced it can be found only by the
    /// session the caller actually saw; scanning for the plan's own, freshly
    /// generated session would never match anything it could have displaced.
    /// An unbound plan observed no launch and scans for the one it proposes.
    pub(super) fn observed_exec_session_id(&self) -> &str {
        self.expected_exec_session_id.as_deref().unwrap_or(&self.exec_session_id)
    }
}

/// How the reservation step answered one prepared launch.
#[derive(Debug)]
pub(in crate::subagents) enum BackgroundRunReservation {
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

/// The bounded message a claim abandoned before its process started settles with.
///
/// The launch path's own guard reports the same text for the same outcome once
/// it takes the claim over.
const BACKGROUND_ABANDONED_CLAIM_ERROR: &str = "Background launch was abandoned before its process started";

/// Settles a claim whose reservation never reached its launch.
///
/// The reservation writes the claim and then awaits its durable write and its
/// publication. The launch arms its own guard only once `Reserved` is returned,
/// so a caller dropped inside either of those awaits would leave a `Starting`
/// record with a non-retryable launch obligation: refresh drives only retryable
/// obligations, so nothing would ever settle it and the record would report an
/// active launch that never spawns. This guard therefore covers the
/// reservation's own awaits. It is disarmed at the end of the reservation; the
/// launch path arms its own guard in the same synchronous stretch, with no
/// await between them, so the claim is guarded continuously from its write to
/// its spawn.
pub(super) struct ReservedClaimGuard {
    state: Arc<RwLock<ControllerState>>,
    record_id: String,
    exec_session_id: String,
    armed: bool,
}

impl ReservedClaimGuard {
    /// Arms the guard for one claim the reservation has just written.
    pub(super) fn arm(state: &Arc<RwLock<ControllerState>>, record_id: &str, exec_session_id: &str) -> Self {
        Self {
            state: Arc::clone(state),
            record_id: record_id.to_owned(),
            exec_session_id: exec_session_id.to_owned(),
            armed: true,
        }
    }

    /// Reports that the reservation answered, so handing the claim to its launch owes nothing.
    pub(super) fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for ReservedClaimGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // The handler runs on the dropping thread, so it must never park the
        // runtime by awaiting. If another task holds the state lock, the record
        // keeps its `Starting` status; the obligation below is still cleared, so
        // the next refresh can settle the orphaned claim instead of exempting it
        // forever.
        if let Ok(mut state) = self.state.try_write() {
            let now = Utc::now();
            if let Some(record) = state.background_children.get_mut(&self.record_id)
                && record.exec_session_id == self.exec_session_id
                && record.desired_enabled
                && record.status == BackgroundSubprocessStatus::Starting
            {
                record.status = BackgroundSubprocessStatus::Error;
                record.error = Some(BACKGROUND_ABANDONED_CLAIM_ERROR.to_string());
                record.summary = Some("Background subagent failed to start".to_string());
                record.updated_at = now;
                record.ended_at.get_or_insert(now);
                // The settlement exists only in memory, so the ledger is owed
                // the write before the lock is released.
                mark_background_ledger_write_pending(&self.state);
            }
        }
        // The driver is gone, so no launch may keep exempting the record: a
        // launch that took it recorded its own obligation under the exec session
        // it minted, and a later refresh drives whatever this claim became.
        clear_background_launch_owed(&self.state, &self.record_id, &self.exec_session_id);
    }
}

pub(super) fn usable_background_parent_session_id(owner: Option<String>) -> Option<String> {
    owner.filter(|owner| !owner.is_empty())
}

/// Reports whether reuse would answer for a launch another parent session drives.
///
/// Reuse returns the existing launch as it is, so the parent a request resolved
/// is dropped on that path: progress keeps routing to the record's parent. The
/// rule must hold for every caller, not only those inside a tool invocation
/// that names a parent, so the resolved parent is compared directly. A record
/// that names a parent the request does not is another session's launch.
pub(super) fn reuse_is_for_another_parent(record_parent_session_id: &str, requested_parent_session_id: &str) -> bool {
    !record_parent_session_id.is_empty() && record_parent_session_id != requested_parent_session_id
}

/// Reports whether reuse must move a parentless record onto the requesting session.
///
/// A record that names no parent predates parent tracking. Nothing routes its
/// progress, and `notify_background_observers` publishes nothing for an empty
/// parent, so a caller told its request was already running would never receive
/// a lifecycle update for it. Reuse therefore rebinds such a record onto the
/// session that asked for it; the reservation holds the write lock the rebind
/// needs, and the ordinary owed write makes it durable. Two empty parents agree
/// and need no rebind.
pub(super) fn reuse_needs_parent_rebind(record_parent_session_id: &str, requested_parent_session_id: &str) -> bool {
    record_parent_session_id.is_empty() && !requested_parent_session_id.is_empty()
}

/// Names the launch settings a record and a desired launch disagree on.
///
/// Reuse is only safe when the record already runs the work the caller asked
/// for, because the launch settings are the identity of that work. A caller
/// that wants a different prompt or model must stop the existing process
/// first: replacing a live launch in place would orphan the process already
/// running.
pub(in crate::subagents) fn background_launch_conflicts(
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
pub(in crate::subagents) fn starting_background_record(plan: &BackgroundRunPlan) -> BackgroundRecord {
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
    pub(super) async fn persisted_record_for(
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

    /// Reports whether the ledger keeps a desired row naming `exec_session_id`.
    ///
    /// A claim is confirmed by its launch identity, not by the id it was
    /// written under: a migrated row is stored under its owner's canonical id,
    /// while the caller still addresses the ownerless one. The exec session is
    /// minted per launch and every saved copy of the claim carries it.
    pub(super) async fn persisted_claim_names(&self, exec_session_id: &str) -> Result<bool> {
        if self.config.managed_background_runtime {
            return Ok(true);
        }
        let persisted = controller_persistence::collect_by_canonical_id(
            load_background_state(&self.config.workspace_root).await?.records,
        );
        Ok(persisted
            .values()
            .any(|row| row.exec_session_id == exec_session_id && persisted_background_row_is_desired(row)))
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
    pub(super) async fn persist_reserved_background_claim(&self, record_id: &str, exec_session_id: &str) -> Result<()> {
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
        controller_persistence::persist_background_records(
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
