//! The launch identity a reservation claims and the answer it gives.

use super::*;

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
    /// The exec session this plan's reservation must recognise as the launch
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

pub(super) fn usable_background_parent_session_id(owner: Option<String>) -> Option<String> {
    owner.filter(|owner| !owner.is_empty())
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
