use super::interview_context::line_has_open_decision_marker;
use super::*;
use crate::agent::runloop::unified::planning_workflow_state::PlanningWorkflowSessionState;
use crate::agent::runloop::unified::state::SessionStats;
use vtcode_core::config::constants::tools;

fn tool_calls_result(tool_calls: Vec<uni::ToolCall>, assistant_text: impl Into<String>) -> TurnProcessingResult {
    TurnProcessingResult::ToolCalls {
        tool_calls: prepare_tool_calls(tool_calls),
        assistant_text: assistant_text.into(),
        reasoning: Vec::new(),
        reasoning_details: None,
    }
}

#[test]
fn maybe_force_planning_workflow_interview_waits_until_discovery_is_ready() {
    let stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    let processing_result = TurnProcessingResult::TextResponse {
        text: "Proceeding with planning research.".to_string(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: None,
    };

    let result = maybe_force_planning_workflow_interview(
        processing_result,
        Some("Proceeding with planning research."),
        &stats,
        &mut plan_session,
        1,
    );
    match result {
        TurnProcessingResult::TextResponse { text, .. } => {
            assert_eq!(text, "Proceeding with planning research.");
        }
        _ => panic!("Expected text response before planning discovery is ready"),
    }
}

#[test]
fn public_discovery_tools_make_planning_interview_ready() {
    for tool in [tools::EXEC_COMMAND, tools::CODE_SEARCH] {
        let mut stats = SessionStats::default();
        let mut plan_session = PlanningWorkflowSessionState::default();
        stats.record_tool(tool);
        plan_session.increment_turns();

        assert!(
            planning_workflow_interview_ready(&stats, &plan_session),
            "{tool} should count as planning discovery"
        );
    }
}

#[test]
fn maybe_force_planning_workflow_interview_appends_reminder_when_plan_ready() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    let processing_result = TurnProcessingResult::TextResponse {
        text: String::new(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: Some("Plan content".to_string()),
    };

    stats.record_tool(tools::READ_FILE);
    plan_session.increment_turns();
    plan_session.record_interview_result(1, false);

    let result = maybe_force_planning_workflow_interview(
        processing_result,
        Some("<proposed_plan>\nPlan content\n</proposed_plan>"),
        &stats,
        &mut plan_session,
        2,
    );
    match result {
        TurnProcessingResult::TextResponse { text, .. } => {
            assert!(text.contains(PLANNING_WORKFLOW_REMINDER));
        }
        _ => panic!("Expected text response with plan reminder"),
    }
}

#[test]
fn maybe_force_planning_workflow_interview_preserves_untagged_plan_candidates() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    let plan = "Summary\nA complete plan without XML tags.\n\nSteps\n1. Gate approval -> files: [src/main.rs] -> verify: cargo check\n\nValidation\n1. Run cargo check.\n\nAssumptions\n1. Keep the current entry point.";
    let processing_result = TurnProcessingResult::TextResponse {
        text: String::new(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: Some(plan.to_string()),
    };

    stats.record_tool(tools::READ_FILE);
    plan_session.increment_turns();

    let result = maybe_force_planning_workflow_interview(processing_result, Some(plan), &stats, &mut plan_session, 2);
    match result {
        TurnProcessingResult::TextResponse { proposed_plan, .. } => {
            assert_eq!(proposed_plan.as_deref(), Some(plan));
        }
        _ => panic!("untagged plan candidate must reach approval handling"),
    }
}

#[test]
fn planning_workflow_reminder_stays_fail_closed_before_persistence() {
    assert!(!PLANNING_WORKFLOW_REMINDER.contains(&format!("/{}", "mode")));
    assert!(!PLANNING_WORKFLOW_REMINDER.contains("implement"));
    assert!(PLANNING_WORKFLOW_REMINDER.contains("Emit exactly one final `<proposed_plan>` block"));
    assert!(
        PLANNING_WORKFLOW_REMINDER
            .contains("Do not use shell commands or file-writing tools to create or modify `.vtcode/plans/`")
    );
    assert!(PLANNING_WORKFLOW_REMINDER.contains("runtime owns plan/tracker persistence and validation"));
    assert!(PLANNING_WORKFLOW_REMINDER.contains("approval controls only after successful persistence"));
}

#[test]
fn maybe_force_planning_workflow_interview_does_not_duplicate_reminder() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    let text = PLANNING_WORKFLOW_REMINDER.to_string();
    let processing_result = TurnProcessingResult::TextResponse {
        text: text.clone(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: Some("Plan content".to_string()),
    };

    stats.record_tool(tools::READ_FILE);
    plan_session.increment_turns();
    plan_session.record_interview_result(1, false);

    let result = maybe_force_planning_workflow_interview(processing_result, Some(&text), &stats, &mut plan_session, 3);
    match result {
        TurnProcessingResult::TextResponse { text, .. } => {
            assert_eq!(text.matches(PLANNING_WORKFLOW_REMINDER).count(), 1);
        }
        _ => panic!("Expected text response with single reminder"),
    }
}

#[test]
fn maybe_force_planning_workflow_interview_preserves_allowed_request_user_input_calls() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    plan_session.increment_turns();
    stats.record_tool(tools::READ_FILE);

    let processing_result = tool_calls_result(
        vec![
            uni::ToolCall::function("call_read".to_string(), tools::READ_FILE.to_string(), "{}".to_string()),
            uni::ToolCall::function(
                "call_interview".to_string(),
                tools::REQUEST_USER_INPUT.to_string(),
                r#"{"questions":[{"id":"scope","header":"Scope","question":"Which scope should the plan cover?","options":[{"label":"Focused","description":"Keep the change narrow."},{"label":"Broad","description":"Include adjacent behaviour."}]}]}"#.to_string(),
            ),
        ],
        String::new(),
    );

    let result = maybe_force_planning_workflow_interview(
        processing_result,
        Some("Going to read files."),
        &stats,
        &mut plan_session,
        3,
    );
    match result {
        TurnProcessingResult::ToolCalls { tool_calls, .. } => {
            assert_eq!(tool_calls.len(), 2);
            assert_eq!(tool_calls[0].tool_name(), tools::READ_FILE);
            assert_eq!(tool_calls[1].tool_name(), tools::REQUEST_USER_INPUT);
            assert!(plan_session.interview_shown());
        }
        _ => panic!("Expected tool calls with request_user_input preserved"),
    }
}

#[test]
fn maybe_force_planning_workflow_interview_injects_fallback_after_discovery() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    stats.record_tool(tools::READ_FILE);
    plan_session.increment_turns();

    let processing_result = TurnProcessingResult::TextResponse {
        text: "The repository evidence is collected.".to_string(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: None,
    };

    let result = maybe_force_planning_workflow_interview(
        processing_result,
        Some("The repository evidence is collected."),
        &stats,
        &mut plan_session,
        4,
    );

    match result {
        TurnProcessingResult::ToolCalls { tool_calls, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].tool_name(), tools::REQUEST_USER_INPUT);
            assert!(tool_calls[0].args().is_some_and(|args| args.get("questions").is_some()));
            assert!(plan_session.interview_shown());
        }
        _ => panic!("Expected a fallback request_user_input tool call"),
    }
}

#[test]
fn maybe_force_planning_workflow_interview_prioritizes_open_decision_before_plan_approval() {
    let mut stats = SessionStats::default();
    let mut plan_session = PlanningWorkflowSessionState::default();
    stats.record_tool(tools::READ_FILE);
    plan_session.increment_turns();

    let text = "<proposed_plan>\nNext open decision: choose the migration scope.\n</proposed_plan>";
    let processing_result = TurnProcessingResult::TextResponse {
        text: text.to_string(),
        reasoning: Vec::new(),
        reasoning_details: None,
        proposed_plan: None,
    };

    let result = maybe_force_planning_workflow_interview(processing_result, Some(text), &stats, &mut plan_session, 5);

    match result {
        TurnProcessingResult::ToolCalls { tool_calls, assistant_text, .. } => {
            assert_eq!(tool_calls.len(), 1);
            assert_eq!(tool_calls[0].tool_name(), tools::REQUEST_USER_INPUT);
            assert!(assistant_text.is_empty());
        }
        _ => panic!("Expected clarification before plan approval"),
    }
}

#[test]
fn line_has_open_decision_marker_only_tracks_next_open_decision() {
    assert!(line_has_open_decision_marker("Next open decision: validate migration order"));
    assert!(!line_has_open_decision_marker("Decision needed: choose validation scope"));
    assert!(!line_has_open_decision_marker("Next open decision: none"));
    assert!(!line_has_open_decision_marker("Next open decision: No remaining scope decisions."));
}
