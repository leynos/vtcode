//! Pure decision predicates for whether the planning-workflow interview
//! should be offered, requested from the model, or forced this turn.
//!
//! Interface contract: every function here is a pure read of
//! [`SessionStats`] / [`PlanningWorkflowSessionState`] / response text — no
//! LLM calls, no tool execution, no mutation of `plan_session`. This keeps
//! the readiness/need decision independently testable and decoupled from
//! orchestration (`super::maybe_force_planning_workflow_interview`), which is
//! the only caller outside this module.
use diapason_core::config::constants::tools;

use super::MIN_PLANNING_WORKFLOW_TURNS_BEFORE_INTERVIEW;
use super::interview_context::has_open_decision_markers;
use crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState;
use crate::agent::runloop::unified::state::SessionStats;

#[derive(Debug, Clone, Copy)]
pub(super) struct InterviewNeedState {
    pub(super) response_has_plan: bool,
    pub(super) needs_interview: bool,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct InterviewGate {
    pub(super) allow_interview: bool,
    pub(super) response_has_plan: bool,
    pub(super) needs_interview: bool,
}

fn has_discovery_tool(session_stats: &SessionStats) -> bool {
    [
        tools::READ_FILE,
        tools::LIST_FILES,
        tools::GREP_FILE,
        tools::UNIFIED_SEARCH,
        tools::UNIFIED_EXEC,
        tools::CODE_SEARCH,
    ]
    .iter()
    .any(|tool| session_stats.has_tool(tool))
}

/// Whether the interview may be shown at all this turn (independent of
/// whether it is *needed* — see [`interview_need_state`]).
pub(crate) fn planning_workflow_interview_ready(
    session_stats: &SessionStats,
    plan_session: &PlanningWorkflowSessionState,
) -> bool {
    // Do NOT allow interview when budget is exhausted — no further LLM calls
    // are possible and re-forcing would loop forever. The same applies when
    // post-tool recovery is exhausted: the planning context is saturated and
    // re-forcing the interview would re-research and loop forever. Likewise,
    // once `request_user_input` has been permanently denied (policy/capability
    // failure, not a user cancellation), re-forcing it would just repeat the
    // same denial every turn (checkpoint turn_655/turn_660).
    if !plan_session.interview_forcing_allowed() {
        return false;
    }
    has_discovery_tool(session_stats) && plan_session.turns() >= MIN_PLANNING_WORKFLOW_TURNS_BEFORE_INTERVIEW
}

pub(super) fn evaluate_interview_gate(
    response_text: Option<&str>,
    response_has_plan: bool,
    session_stats: &SessionStats,
    plan_session: &PlanningWorkflowSessionState,
) -> InterviewGate {
    let need_state = interview_need_state(response_text, response_has_plan, plan_session);
    InterviewGate {
        allow_interview: planning_workflow_interview_ready(session_stats, plan_session),
        response_has_plan: need_state.response_has_plan,
        needs_interview: need_state.needs_interview,
    }
}

/// Whether the planning session still needs an interview cycle, and whether
/// the response already contains a validated-shape plan candidate.
pub(super) fn interview_need_state(
    response_text: Option<&str>,
    response_has_plan: bool,
    plan_session: &PlanningWorkflowSessionState,
) -> InterviewNeedState {
    let has_open_decisions = response_text.map(has_open_decision_markers).unwrap_or(false);
    let has_completed_interview = plan_session.interview_cycles_completed() > 0;
    let interview_cancelled = plan_session.last_interview_cancelled();

    // A complete response candidate is already actionable and should flow to
    // the approval gate instead of being discarded while an interview is
    // injected. Open decisions remain the exception: they must be resolved by
    // the interview before approval can proceed.
    let needs_interview =
        has_open_decisions || (!response_has_plan && (!has_completed_interview || interview_cancelled));

    InterviewNeedState { response_has_plan, needs_interview }
}
