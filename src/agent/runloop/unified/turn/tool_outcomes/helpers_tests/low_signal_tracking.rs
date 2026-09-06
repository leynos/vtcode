//! Low-signal search and read outcome tracking tests.

use super::*;

#[test]
fn low_signal_tracker_groups_empty_search_results_by_family() {
    let mut tracker = LoopTracker::new();
    let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({"results":[]}),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    // Different queries produce separate family keys, so each counts as its
    // own family while the agent explores one path.
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::CODE_SEARCH,
        &json!({"query":"Widget", "path":"src", "result_types":["definition"]}),
    );
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::CODE_SEARCH,
        &json!({"query":"Result", "path":"src", "result_types":["usage"]}),
    );
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::CODE_SEARCH,
        &json!({"query":"Result<", "path":"src", "result_types":["text"]}),
    );

    assert_eq!(tracker.max_low_signal_count(), 1);
}

#[test]
fn low_signal_tracker_groups_identical_searches_in_same_family() {
    let mut tracker = LoopTracker::new();
    let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({"results":[]}),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    let args = json!({"query":"TODO","path":"src","file_types":["rust"]});
    update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);
    update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);
    update_repetition_tracker(&mut tracker, &miss, tools::CODE_SEARCH, &args);

    assert_eq!(tracker.max_low_signal_count(), 3);
}

#[test]
fn low_signal_tracker_ignores_empty_search_results_with_recovery_guidance() {
    let mut tracker = LoopTracker::new();
    let guided = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "results": [],
            "hint": "Try narrowing the path.",
            "is_recoverable": true,
            "next_action": "Retry with narrower filters."
        }),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    update_repetition_tracker(
        &mut tracker,
        &guided,
        tools::CODE_SEARCH,
        &json!({"query":"run", "path":"src/agent", "result_types":["definition"]}),
    );

    assert_eq!(tracker.max_low_signal_count(), 0);
}

#[test]
fn low_signal_tracker_does_not_hide_structured_search_errors_as_empty_results() {
    let mut tracker = LoopTracker::new();
    let failure_like = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "results": [],
            "error": "permission denied while searching the workspace"
        }),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    update_repetition_tracker(
        &mut tracker,
        &failure_like,
        tools::CODE_SEARCH,
        &json!({"query":"secret", "path":"src"}),
    );

    assert_eq!(tracker.max_low_signal_count(), 0);
    assert_eq!(tracker.consecutive_low_signal_navigations, 0);
}

#[test]
fn low_signal_tracker_counts_missing_read_failures() {
    let mut tracker = LoopTracker::new();
    let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: vtcode_core::tools::registry::ToolExecutionError::new(
            tools::UNIFIED_FILE.to_string(),
            vtcode_core::tools::registry::ToolErrorType::ResourceNotFound,
            "Resource not found: vtcode-tui/src/main.rs".to_string(),
        ),
    });

    // Two reads of the same path with different offsets are *different*
    // slices (paginated exploration), not a retry loop. The slice-aware
    // family key keeps them as distinct families, each with count 1.
    // Regression: previously both collapsed into one family with count 2,
    // which falsely tripped the family cap when the model paginated a
    // missing file (checkpoint turn_613 pattern).
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"vtcode-tui/src/main.rs"}),
    );
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::UNIFIED_FILE,
        &json!({"action":"read","path":"vtcode-tui/src/main.rs","offset":40}),
    );

    assert_eq!(
        tracker.max_low_signal_count(),
        1,
        "paginated reads (different offset) must be distinct families, not one family with count 2"
    );
}

#[test]
fn low_signal_tracker_counts_identical_missing_read_failures() {
    // True retry loop: same path + same slice, repeated. The low-signal
    // count must accumulate so the turn balancer can stop the churn.
    let mut tracker = LoopTracker::new();
    let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: vtcode_core::tools::registry::ToolExecutionError::new(
            tools::UNIFIED_FILE.to_string(),
            vtcode_core::tools::registry::ToolErrorType::ResourceNotFound,
            "Resource not found: vtcode-tui/src/main.rs".to_string(),
        ),
    });

    let identical_args = json!({"action":"read","path":"vtcode-tui/src/main.rs"});
    update_repetition_tracker(&mut tracker, &miss, tools::UNIFIED_FILE, &identical_args);
    update_repetition_tracker(&mut tracker, &miss, tools::UNIFIED_FILE, &identical_args);

    assert_eq!(
        tracker.max_low_signal_count(),
        2,
        "identical retry reads must accumulate into one family with count 2"
    );
}

#[test]
fn low_signal_tracker_counts_grep_style_shell_misses() {
    let mut tracker = LoopTracker::new();
    let miss = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "command": "grep -n 'missing' vtcode-tui/src/main.rs",
            "exit_code": 1,
            "output": ""
        }),
        stdout: None,
        modified_files: vec![],
        command_success: false,
    });
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::EXEC_COMMAND,
        &json!({"cmd":"grep -n 'missing' vtcode-tui/src/main.rs"}),
    );
    update_repetition_tracker(
        &mut tracker,
        &miss,
        tools::EXEC_COMMAND,
        &json!({"cmd":"grep -n \"missing\" vtcode-tui/src/main.rs"}),
    );

    assert_eq!(tracker.max_low_signal_count(), 2);
    assert_eq!(tracker.consecutive_low_signal_navigations, 2);
    assert_eq!(tracker.total_low_signal_navigations, 2);
}

#[test]
fn low_signal_tracker_does_not_count_grep_style_errors_as_no_match() {
    let mut tracker = LoopTracker::new();
    let error = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "command": "rg missing restricted",
            "exit_code": 2,
            "output": ""
        }),
        stdout: None,
        modified_files: vec![],
        command_success: false,
    });

    update_repetition_tracker(&mut tracker, &error, tools::EXEC_COMMAND, &json!({"cmd":"rg missing restricted"}));

    assert_eq!(tracker.max_low_signal_count(), 0);
    assert_eq!(tracker.consecutive_low_signal_navigations, 0);
    assert_eq!(tracker.total_low_signal_navigations, 0);
}

#[test]
fn low_signal_tracker_does_not_hide_grep_errors_as_no_match() {
    let mut tracker = LoopTracker::new();
    let failure = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "command": "rg missing restricted",
            "exit_code": 1,
            "stdout": "",
            "stderr": "permission denied",
        }),
        stdout: None,
        modified_files: vec![],
        command_success: false,
    });

    update_repetition_tracker(&mut tracker, &failure, tools::EXEC_COMMAND, &json!({"cmd":"rg missing restricted"}));

    assert_eq!(tracker.max_low_signal_count(), 0);
    assert_eq!(tracker.consecutive_low_signal_navigations, 0);
}
