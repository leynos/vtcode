use crate::agent::runloop::unified::state::SessionStats;
use anyhow::Result;
use vtcode_commons::ui_protocol::ActivityState;
use vtcode_config::builtin_primary_build_agent;
use vtcode_core::core::interfaces::session::PlanningEntrySource;
use vtcode_core::tools::registry::ToolRegistry;
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_ui::tui::app::InlineHandle;

#[derive(Default)]
pub(crate) struct PlanningWorkflowSessionState {
    interview_shown: bool,
    interview_pending: bool,
    turns: usize,
    interview_cycles_completed: usize,
    last_interview_cancelled: bool,
    entry_source: Option<PlanningEntrySource>,
    /// Set when the session budget is exhausted during planning. Prevents
    /// the interview from being re-forced on the next turn, which would
    /// loop forever because no further LLM calls are possible.
    budget_exhausted: bool,
    /// Set when the post-tool recovery cycle cap is reached during planning
    /// (repeated tool-free synthesis failures because the planning context is
    /// saturated). Prevents the interview from being re-forced on the next
    /// turn, which would re-research the still-huge context and fail again —
    /// looping forever across turns.
    recovery_exhausted: bool,
    /// Set when a `request_user_input` tool call is denied by a permanent
    /// capability/policy failure (e.g. the tool is not available in the
    /// current runtime) rather than the user cancelling the modal. Unlike
    /// cancellation, a policy denial will recur on every retry — this flag
    /// permanently stops the interview from being re-forced for the rest of
    /// the planning session, falling back to autonomous plan synthesis
    /// instead of looping (see checkpoint turn_655/turn_660).
    interview_denied: bool,
    /// Allows one bounded synthesis retry after an interview denial. The
    /// retry gives the model a direct instruction to emit a completed plan
    /// from the research already gathered instead of ending with an approval
    /// hint that has no draft behind it.
    plan_synthesis_retry_used: bool,
    /// Counts automatic validation-repair prompts issued during the current
    /// planning turn. The counter is deliberately turn-scoped so a failed
    /// draft cannot consume the repair budget for every later user turn.
    plan_validation_repair_reprompts: u8,
    /// Number of validation-repair requests waiting to be admitted through
    /// the turn loop. This is independent of the number of text responses
    /// already emitted: a repair may be scheduled after an interview denial
    /// or another ordinary planning response has used that budget.
    plan_validation_repair_follow_ups_pending: u8,
    /// Counts re-prompts issued after the model emitted pseudo-tool-call
    /// markup (XML-ish tool-call text no parser could execute) as a plan-mode
    /// text response. Bounded so a checkpoint that keeps emitting the same
    /// markup cannot loop the turn forever (turn_887/turn_888).
    pseudo_tool_call_reprompts: u32,
    /// Primary agent that was active before the planning workflow began.
    /// Used to restore execution to the prior mode when planning was entered
    /// by selecting the dedicated plan agent.
    previous_primary_agent: Option<String>,
    /// Configured execution agent to use when planning started from the
    /// dedicated `plan` agent without a previous execution agent.
    fallback_primary_agent: Option<String>,
    /// Telemetry identity for the latest unresolved plan approval request.
    pending_approval: Option<PendingPlanApproval>,
}

/// Maximum number of pseudo-tool-call-markup re-prompts per planning session.
/// One re-prompt usually teaches the model to use the real tool-call channel;
/// two covers a repeat offense. Beyond that the turn ends with the cleaned
/// text so the user can steer.
pub(crate) const MAX_PLAN_PSEUDO_TOOL_CALL_REPROMPTS: u32 = 2;

/// Maximum number of automatic validation-repair prompts after the initial
/// invalid plan candidate in one planning turn.
pub(crate) const MAX_PLAN_VALIDATION_REPAIR_REPROMPTS: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPlanApproval {
    pub(crate) thread_id: String,
    pub(crate) turn_id: String,
}

impl PlanningWorkflowSessionState {
    pub(crate) fn enter(&mut self, entry_source: PlanningEntrySource) {
        self.interview_shown = false;
        self.interview_pending = false;
        self.turns = 0;
        self.interview_cycles_completed = 0;
        self.last_interview_cancelled = false;
        self.entry_source = Some(entry_source);
        self.budget_exhausted = false;
        self.recovery_exhausted = false;
        self.interview_denied = false;
        self.plan_synthesis_retry_used = false;
        self.plan_validation_repair_reprompts = 0;
        self.plan_validation_repair_follow_ups_pending = 0;
        self.pseudo_tool_call_reprompts = 0;
        self.previous_primary_agent = None;
        self.fallback_primary_agent = None;
        self.pending_approval = None;
    }

    pub(crate) fn exit(&mut self) {
        self.entry_source = None;
        self.budget_exhausted = false;
        self.recovery_exhausted = false;
        self.interview_denied = false;
        self.plan_synthesis_retry_used = false;
        self.plan_validation_repair_reprompts = 0;
        self.plan_validation_repair_follow_ups_pending = 0;
        self.pseudo_tool_call_reprompts = 0;
        self.previous_primary_agent = None;
        self.fallback_primary_agent = None;
        self.pending_approval = None;
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    #[cfg(test)]
    pub(crate) fn interview_shown(&self) -> bool {
        self.interview_shown
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn mark_interview_shown(&mut self) {
        self.interview_shown = true;
        self.interview_pending = false;
    }

    pub(crate) fn turns(&self) -> usize {
        self.turns
    }

    pub(crate) fn increment_turns(&mut self) {
        self.turns = self.turns.saturating_add(1);
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn interview_pending(&self) -> bool {
        self.interview_pending
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn mark_interview_pending(&mut self) {
        self.interview_pending = true;
    }

    pub(crate) fn clear_interview_pending(&mut self) {
        self.interview_pending = false;
    }

    pub(crate) fn record_interview_result(&mut self, answered_questions: usize, cancelled: bool) {
        let answered_questions = answered_questions.min(3);
        self.last_interview_cancelled = cancelled || answered_questions == 0;
        self.interview_pending = false;

        if !self.last_interview_cancelled {
            self.interview_cycles_completed = self.interview_cycles_completed.saturating_add(1);
            self.interview_shown = true;
        } else {
            self.interview_shown = false;
        }
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn interview_cycles_completed(&self) -> usize {
        self.interview_cycles_completed
    }

    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) fn last_interview_cancelled(&self) -> bool {
        self.last_interview_cancelled
    }

    pub(crate) fn mark_budget_exhausted(&mut self) {
        self.budget_exhausted = true;
    }

    pub(crate) fn is_budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    pub(crate) fn mark_recovery_exhausted(&mut self) {
        self.recovery_exhausted = true;
    }

    pub(crate) fn is_recovery_exhausted(&self) -> bool {
        self.recovery_exhausted
    }

    /// Record that `request_user_input` was denied by a permanent
    /// capability/policy failure this session. Once set, the interview must
    /// never be re-forced — see the field doc comment for why this differs
    /// from `record_interview_result(0, cancelled=true)`.
    pub(crate) fn mark_interview_denied(&mut self) {
        self.interview_denied = true;
        self.interview_pending = false;
    }

    pub(crate) fn is_interview_denied(&self) -> bool {
        self.interview_denied
    }

    pub(crate) fn plan_synthesis_retry_allowed(&self) -> bool {
        self.interview_denied && !self.plan_synthesis_retry_used && !self.budget_exhausted && !self.recovery_exhausted
    }

    pub(crate) fn mark_plan_synthesis_retry_used(&mut self) {
        self.plan_synthesis_retry_used = true;
        self.interview_pending = false;
    }

    /// Reset the automatic validation-repair budget for a fresh planning turn.
    pub(crate) fn start_turn(&mut self) {
        self.plan_validation_repair_reprompts = 0;
        self.plan_validation_repair_follow_ups_pending = 0;
    }

    pub(crate) fn plan_validation_repair_allowed(&self) -> bool {
        self.plan_validation_repair_reprompts < MAX_PLAN_VALIDATION_REPAIR_REPROMPTS
    }

    pub(crate) fn plan_validation_repair_follow_up_allowed(&self) -> bool {
        self.plan_validation_repair_follow_ups_pending > 0
    }

    pub(crate) fn consume_plan_validation_repair_follow_up(&mut self) {
        self.plan_validation_repair_follow_ups_pending =
            self.plan_validation_repair_follow_ups_pending.saturating_sub(1);
    }

    pub(crate) fn mark_plan_validation_repair_used(&mut self) {
        self.plan_validation_repair_reprompts = self.plan_validation_repair_reprompts.saturating_add(1);
        self.plan_validation_repair_follow_ups_pending =
            self.plan_validation_repair_follow_ups_pending.saturating_add(1);
    }

    pub(crate) fn plan_pseudo_tool_call_reprompt_allowed(&self) -> bool {
        self.pseudo_tool_call_reprompts < MAX_PLAN_PSEUDO_TOOL_CALL_REPROMPTS
    }

    pub(crate) fn mark_plan_pseudo_tool_call_reprompt_used(&mut self) {
        self.pseudo_tool_call_reprompts += 1;
    }

    pub(crate) fn interview_forcing_allowed(&self) -> bool {
        !self.is_budget_exhausted() && !self.is_recovery_exhausted() && !self.is_interview_denied()
    }

    pub(crate) fn set_previous_primary_agent(&mut self, agent: Option<String>) {
        self.previous_primary_agent = agent.filter(|name| !name.trim().is_empty());
    }

    pub(crate) fn set_fallback_primary_agent(&mut self, agent: Option<String>) {
        self.fallback_primary_agent = agent.filter(|name| !name.trim().is_empty());
    }

    pub(crate) fn previous_primary_agent(&self) -> Option<&str> {
        self.previous_primary_agent.as_deref()
    }

    /// Resolve the primary agent that should execute an approved plan.
    ///
    /// Planning may be entered from an execution agent or by selecting the
    /// dedicated `plan` agent. Keep this decision at the planning-state
    /// boundary so inline, headless, and automatic approval paths cannot drift
    /// into different handoff behaviour.
    pub(crate) fn execution_agent_after_approval(&self, active_agent_name: &str) -> Option<String> {
        if let Some(previous) = self
            .previous_primary_agent()
            .filter(|agent| !agent.eq_ignore_ascii_case(active_agent_name) && !agent.eq_ignore_ascii_case("plan"))
        {
            return Some(previous.to_owned());
        }

        active_agent_name.eq_ignore_ascii_case("plan").then(|| {
            self.fallback_primary_agent
                .clone()
                .unwrap_or_else(|| builtin_primary_build_agent().name)
        })
    }

    pub(crate) fn mark_plan_approval_pending(&mut self, thread_id: String, turn_id: String) {
        self.pending_approval = Some(PendingPlanApproval { thread_id, turn_id });
    }

    pub(crate) fn take_pending_plan_approval(&mut self) -> Option<PendingPlanApproval> {
        self.pending_approval.take()
    }
}

pub(crate) const PLANNING_WORKFLOW_REVIEW_AND_EXECUTE_HINT: &str = "Planning workflow is active. Continue refining the plan; approval controls appear only after a validated draft is persisted.";
pub(crate) const PLANNING_WORKFLOW_SHORT_CONFIRMATION_HINT: &str = "Planning workflow: type `implement` (or `yes`/`continue`/`go`/`start`) to execute, or say `keep planning` to revise.";
pub(crate) const PLANNING_WORKFLOW_KEEP_PLANNING_HINT: &str =
    "To keep planning, say `keep planning` and describe what to revise.";
pub(crate) const PLANNING_WORKFLOW_MANUAL_SWITCH_FALLBACK_HINT: &str =
    "If the persisted plan is not shown automatically, type `implement` to present it for approval.";
pub(crate) const PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT: &str =
    "Planning workflow remains active: no approval-ready plan was produced. Keep planning and describe what to revise.";

pub(crate) fn short_confirmation_hint_with_fallback() -> String {
    format!("{PLANNING_WORKFLOW_SHORT_CONFIRMATION_HINT} {PLANNING_WORKFLOW_MANUAL_SWITCH_FALLBACK_HINT}")
}

pub(crate) fn render_planning_workflow_next_step_hint(renderer: &mut AnsiRenderer) -> Result<()> {
    renderer.line(MessageStyle::Info, PLANNING_WORKFLOW_REVIEW_AND_EXECUTE_HINT)?;
    renderer.line(MessageStyle::Info, PLANNING_WORKFLOW_KEEP_PLANNING_HINT)?;
    renderer.line(MessageStyle::Info, PLANNING_WORKFLOW_NO_APPROVAL_READY_PLAN_HINT)?;
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlanningFinishReason {
    Approved,
    Cancelled,
}

pub(crate) async fn transition_to_planning_workflow(
    tool_registry: &ToolRegistry,
    session_stats: &mut SessionStats,
    plan_session: &mut PlanningWorkflowSessionState,
    handle: &InlineHandle,
    entry_source: PlanningEntrySource,
    previous_primary_agent: Option<String>,
    fallback_primary_agent: Option<String>,
    reset_plan_file: bool,
    reset_plan_baseline: bool,
) {
    tool_registry.enable_planning();
    tool_registry.apply_planning_mode_policy_overrides().await;
    let plan_state = tool_registry.planning_workflow_state();
    // `enable_planning()` above already sets the active flag on
    // `PlanningWorkflowState` (the single source of truth), so we do not call
    // `plan_state.enable()` again here.
    if reset_plan_file {
        plan_state.set_plan_file(None).await;
    }
    if reset_plan_baseline {
        plan_state.set_plan_baseline(None).await;
    }

    session_stats.reset_for_planning_workflow_entry();
    plan_session.enter(entry_source);
    plan_session.set_previous_primary_agent(previous_primary_agent);
    plan_session.set_fallback_primary_agent(fallback_primary_agent);
    handle.set_activity_state(ActivityState::Planning);
    handle.force_redraw();
}

pub(crate) async fn finish_planning_workflow(
    tool_registry: &ToolRegistry,
    plan_session: &mut PlanningWorkflowSessionState,
    handle: &InlineHandle,
    reason: PlanningFinishReason,
) -> Result<Option<crate::agent::runloop::unified::planning_workflow::TaskTrackerHandoff>> {
    let tracker = if reason == PlanningFinishReason::Approved {
        Some(
            crate::agent::runloop::unified::planning_workflow::create_task_tracker_from_active_plan(
                tool_registry,
                handle,
            )
            .await?,
        )
    } else {
        None
    };
    tool_registry.disable_planning();
    tool_registry.restore_post_planning_policies().await;
    let plan_state = tool_registry.planning_workflow_state();
    plan_state.disable();
    if reason == PlanningFinishReason::Cancelled {
        plan_state.set_plan_file(None).await;
    }

    plan_session.exit();
    handle.set_activity_state(ActivityState::Idle);
    handle.force_redraw();
    Ok(tracker)
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_PLAN_PSEUDO_TOOL_CALL_REPROMPTS, MAX_PLAN_VALIDATION_REPAIR_REPROMPTS, PlanningWorkflowSessionState,
    };
    use vtcode_core::core::interfaces::session::PlanningEntrySource;

    #[test]
    fn interview_result_updates_cycle_metrics() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);

        state.record_interview_result(2, false);
        assert_eq!(state.interview_cycles_completed(), 1);
        assert!(!state.last_interview_cancelled());

        state.record_interview_result(0, true);
        assert_eq!(state.interview_cycles_completed(), 1);
        assert!(state.last_interview_cancelled());
    }

    #[test]
    fn entering_resets_interview_cycle_metrics() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        state.record_interview_result(1, false);
        assert_eq!(state.interview_cycles_completed(), 1);

        state.exit();
        state.enter(PlanningEntrySource::UserRequest);
        assert_eq!(state.interview_cycles_completed(), 0);
        assert!(!state.last_interview_cancelled());
    }

    #[test]
    fn mark_interview_denied_is_permanent_until_reset() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(!state.is_interview_denied());

        state.mark_interview_pending();
        state.mark_interview_denied();
        assert!(state.is_interview_denied());
        // A denial also clears any pending interview request — re-forcing it
        // would just repeat the same policy failure.
        assert!(!state.interview_pending());

        // Re-entering the planning workflow (a fresh session) clears the flag.
        state.exit();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(!state.is_interview_denied());
    }

    #[test]
    fn interview_denial_allows_one_plan_synthesis_retry() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        state.mark_interview_denied();

        assert!(state.plan_synthesis_retry_allowed());
        state.mark_plan_synthesis_retry_used();
        assert!(!state.plan_synthesis_retry_allowed());

        state.exit();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(!state.is_interview_denied());
        assert!(!state.plan_synthesis_retry_allowed());
    }

    #[test]
    fn pseudo_tool_call_reprompts_are_bounded_and_reset_by_enter() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);

        for attempt in 0..MAX_PLAN_PSEUDO_TOOL_CALL_REPROMPTS {
            assert!(
                state.plan_pseudo_tool_call_reprompt_allowed(),
                "reprompt attempt {attempt} should be allowed before the bound is reached"
            );
            state.mark_plan_pseudo_tool_call_reprompt_used();
        }
        assert!(
            !state.plan_pseudo_tool_call_reprompt_allowed(),
            "reprompts must stop once the bound is exhausted so a checkpoint that keeps emitting markup cannot loop the turn"
        );

        state.exit();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(state.plan_pseudo_tool_call_reprompt_allowed());
    }

    #[test]
    fn plan_validation_repair_is_bounded_and_reset_at_turn_and_reentry() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        for attempt in 0..MAX_PLAN_VALIDATION_REPAIR_REPROMPTS {
            assert!(
                state.plan_validation_repair_allowed(),
                "repair attempt {attempt} should be allowed before the bound is reached"
            );
            state.mark_plan_validation_repair_used();
        }
        assert!(
            !state.plan_validation_repair_allowed(),
            "validation repairs must stop after the bounded automatic passes"
        );
        assert!(state.plan_validation_repair_follow_up_allowed());
        state.consume_plan_validation_repair_follow_up();
        assert!(state.plan_validation_repair_follow_up_allowed());
        state.consume_plan_validation_repair_follow_up();
        assert!(!state.plan_validation_repair_follow_up_allowed());

        state.start_turn();
        assert!(state.plan_validation_repair_allowed(), "a fresh planning turn gets a fresh repair budget");
        assert!(!state.plan_validation_repair_follow_up_allowed(), "a fresh turn has no stale repair request");
        state.mark_plan_validation_repair_used();
        assert!(
            state.plan_validation_repair_follow_up_allowed(),
            "a repair remains pending regardless of earlier text responses"
        );

        state.exit();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(state.plan_validation_repair_allowed());
        assert!(!state.plan_validation_repair_follow_up_allowed(), "re-entry clears pending repair requests");
    }

    #[test]
    fn budget_and_recovery_exhaustion_cleared_by_enter_and_exit() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);

        state.mark_budget_exhausted();
        state.mark_recovery_exhausted();
        assert!(state.is_budget_exhausted());
        assert!(state.is_recovery_exhausted());

        // exit() clears both exhaustion flags.
        state.exit();
        assert!(!state.is_budget_exhausted());
        assert!(!state.is_recovery_exhausted());

        // Re-apply and verify enter() also clears them.
        state.mark_budget_exhausted();
        state.mark_recovery_exhausted();
        state.enter(PlanningEntrySource::UserRequest);
        assert!(!state.is_budget_exhausted());
        assert!(!state.is_recovery_exhausted());
    }

    #[test]
    fn record_interview_result_treats_zero_answered_as_cancelled() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);

        // answered_questions=0 with cancelled=false should still count as cancelled.
        state.record_interview_result(0, false);
        assert!(state.last_interview_cancelled());
        assert_eq!(state.interview_cycles_completed(), 0);
    }

    #[test]
    fn record_interview_result_clamps_answered_questions_to_three() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);

        state.record_interview_result(10, false);
        assert_eq!(state.interview_cycles_completed(), 1);
        assert!(!state.last_interview_cancelled());
    }

    #[test]
    fn previous_primary_agent_is_retained_only_for_active_planning_session() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::AgentSelection);
        state.set_previous_primary_agent(Some("build".to_string()));

        assert_eq!(state.previous_primary_agent(), Some("build"));

        state.exit();
        assert_eq!(state.previous_primary_agent(), None);
    }

    #[test]
    fn approved_plan_restores_prior_agent_or_falls_back_from_plan_agent() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::AgentSuggestion);
        state.set_previous_primary_agent(Some("auto".to_string()));

        assert_eq!(state.execution_agent_after_approval("plan"), Some("auto".to_string()));

        state.set_previous_primary_agent(None);
        assert_eq!(state.execution_agent_after_approval("plan"), Some("build".to_string()));
        assert_eq!(state.execution_agent_after_approval("auto"), None);
    }

    #[test]
    fn configured_execution_fallback_wins_over_builtin_build() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        state.set_fallback_primary_agent(Some("duck".to_string()));

        assert_eq!(state.execution_agent_after_approval("plan"), Some("duck".to_string()));
    }

    #[test]
    fn pending_approval_identity_is_consumed_once() {
        let mut state = PlanningWorkflowSessionState::default();
        state.enter(PlanningEntrySource::UserRequest);
        state.mark_plan_approval_pending("thread-1".to_string(), "turn-2".to_string());

        assert_eq!(
            state.take_pending_plan_approval(),
            Some(super::PendingPlanApproval {
                thread_id: "thread-1".to_string(),
                turn_id: "turn-2".to_string(),
            })
        );
        assert_eq!(state.take_pending_plan_approval(), None);
    }
}
