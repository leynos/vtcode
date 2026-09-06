//! Verification pressure and mutation-gate tests.

use super::*;

#[test]
fn repetition_tracker_counts_failures() {
    let mut tracker = LoopTracker::new();
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: vtcode_core::tools::registry::ToolExecutionError::new(
            "edit_file".to_string(),
            vtcode_core::tools::registry::ToolErrorType::ExecutionError,
            "boom".to_string(),
        ),
    });

    update_repetition_tracker(&mut tracker, &outcome, "edit_file", &json!({"path":"src/main.rs"}));

    assert_eq!(tracker.max_count_filtered(|_| false), 1);
}

#[test]
fn failed_file_mutations_do_not_trigger_verification_pressure() {
    let mut tracker = LoopTracker::new();
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: vtcode_core::tools::registry::ToolExecutionError::new(
            "apply_patch".to_string(),
            vtcode_core::tools::registry::ToolErrorType::ExecutionError,
            "invalid patch path".to_string(),
        ),
    });

    update_repetition_tracker(
        &mut tracker,
        &outcome,
        tools::APPLY_PATCH,
        &json!({"input":"*** Begin Patch\n*** Update File: /absolute/path\n*** End Patch"}),
    );

    assert_eq!(tracker.consecutive_mutations, 0);
}

#[test]
fn no_op_write_does_not_trigger_verification_pressure() {
    let mut tracker = LoopTracker::new();
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: json!({
            "success": true,
            "path": "README.md",
            "diff_preview": {
                "content": "",
                "truncated": false,
                "omitted_line_count": 0,
                "skipped": false,
                "is_empty": true
            },
            "diff": [{
                "path": "README.md",
                "content": "",
                "truncated": false,
                "omitted_line_count": 0,
                "skipped": false,
                "is_empty": true
            }]
        }),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    update_repetition_tracker(
        &mut tracker,
        &outcome,
        tools::WRITE_FILE,
        &json!({"path":"README.md","content":"same\n","mode":"overwrite"}),
    );

    assert_eq!(tracker.consecutive_mutations, 0);
}

#[test]
fn skipped_write_does_not_trigger_verification_pressure() {
    let mut tracker = LoopTracker::new();
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: json!({
            "success": true,
            "skipped": true,
            "reason": "File already exists"
        }),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });

    update_repetition_tracker(
        &mut tracker,
        &outcome,
        tools::WRITE_FILE,
        &json!({"path":"README.md","content":"same\n","mode":"skip_if_exists"}),
    );

    assert_eq!(tracker.consecutive_mutations, 0);
}

#[test]
fn verification_gate_blocks_mutations_but_allows_reads_checks_and_plan_artefacts() {
    let mut tracker = LoopTracker::new();
    tracker.verification_pending = true;

    assert!(mutation_blocked_until_verification(
        &tracker,
        tools::WRITE_FILE,
        &json!({"path":"README.md","content":"new"})
    ));
    assert!(mutation_blocked_until_verification(
        &tracker,
        tools::EXEC_COMMAND,
        &json!({"cmd":"sed -i '' 's/old/new/' README.md"})
    ));
    assert!(!mutation_blocked_until_verification(&tracker, tools::READ_FILE, &json!({"path":"README.md"})));
    assert!(!mutation_blocked_until_verification(
        &tracker,
        tools::EXEC_COMMAND,
        &json!({"cmd":"cargo check --locked"})
    ));
    assert!(!mutation_blocked_until_verification(
        &tracker,
        tools::WRITE_FILE,
        &json!({"path":".vtcode/plans/next.md","content":"plan"})
    ));
    assert!(!mutation_blocked_until_verification(&tracker, tools::TASK_TRACKER, &json!({"action":"update"})));
}

#[test]
fn inspection_does_not_clear_mutations_waiting_for_verification() {
    let mut tracker = LoopTracker::new();
    let success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({}),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;

    for command in ["git diff -- README.md", "git diff --check"] {
        update_repetition_tracker(&mut tracker, &success, tools::EXEC_COMMAND, &json!({"cmd":command}));
    }

    assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
}

#[test]
fn failed_verification_does_not_clear_mutations_waiting_for_verification() {
    let mut tracker = LoopTracker::new();
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
    tracker.verification_pending = true;
    let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({"exit_code": 1}),
        stdout: None,
        modified_files: vec![],
        command_success: false,
    });

    update_repetition_tracker(&mut tracker, &failed_check, tools::EXEC_COMMAND, &json!({"cmd":"cargo check"}));

    assert!(tracker.verification_is_pending());
    assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
    // A failed verifier keeps the gate but opens a bounded fix-up window
    // so the broken build can be repaired instead of deadlocking.
    assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);
    assert!(!mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
}

#[test]
fn failed_verification_fix_window_is_consumed_by_repair_edits() {
    let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
    let failed_check = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({"exit_code": 1}),
        stdout: None,
        modified_files: vec![],
        command_success: false,
    });
    update_repetition_tracker(&mut tracker, &failed_check, tools::EXEC_COMMAND, &json!({"cmd":"cargo check"}));
    assert_eq!(tracker.fix_edits_remaining, FAILED_VERIFICATION_FIX_ALLOWANCE);

    let edit = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({}),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });
    for _ in 0..FAILED_VERIFICATION_FIX_ALLOWANCE {
        assert!(!mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
        update_repetition_tracker(&mut tracker, &edit, tools::EDIT_FILE, &json!({"path": "src/lib.rs"}));
        assert!(tracker.verification_is_pending());
    }
    // Window exhausted: further mutations block again until a standalone
    // verifier succeeds.
    assert_eq!(tracker.fix_edits_remaining, 0);
    assert!(mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
}

#[test]
fn piped_verifier_is_admitted_but_does_not_clear_gate() {
    let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
    // Piped verifiers must run (not block) so the model sees output, but
    // the pipeline status is the truncator's — only standalone success clears.
    assert!(!mutation_blocked_until_verification(
        &tracker,
        tools::EXEC_COMMAND,
        &json!({"cmd": "cargo check --locked 2>&1 | head -c 4000"})
    ));
    let piped_success = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({"exit_code": 0}),
        stdout: None,
        modified_files: vec![],
        command_success: true,
    });
    update_repetition_tracker(
        &mut tracker,
        &piped_success,
        tools::EXEC_COMMAND,
        &json!({"cmd": "cargo check --locked 2>&1 | head -c 4000"}),
    );
    assert!(tracker.verification_is_pending());
    assert_eq!(tracker.consecutive_mutations, BLIND_EDITING_THRESHOLD);
}

#[test]
fn smuggled_mutation_behind_verifier_prefix_stays_blocked() {
    let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
    for command in [
        "cargo check && rm -rf target",
        "cargo check; rm foo.txt",
        "cargo check --locked && cargo test && rm foo.txt",
    ] {
        assert!(
            mutation_blocked_until_verification(&tracker, tools::EXEC_COMMAND, &json!({"cmd": command})),
            "smuggled mutation must stay blocked: {command}"
        );
    }
}

#[test]
fn tool_level_verification_failure_grants_no_fix_window() {
    let mut tracker = LoopTracker::with_verification_snapshot((true, 0));
    tracker.consecutive_mutations = BLIND_EDITING_THRESHOLD;
    let tool_failure = ToolPipelineOutcome::from_status(ToolExecutionStatus::Failure {
        error: vtcode_core::tools::registry::ToolExecutionError::new(
            tools::EXEC_COMMAND.to_string(),
            vtcode_core::tools::registry::ToolErrorType::ExecutionError,
            "check could not start".to_string(),
        ),
    });
    update_repetition_tracker(
        &mut tracker,
        &tool_failure,
        tools::EXEC_COMMAND,
        &json!({"cmd": "cargo check --locked"}),
    );
    assert!(tracker.verification_is_pending());
    assert_eq!(tracker.fix_edits_remaining, 0);
    assert!(mutation_blocked_until_verification(&tracker, tools::EDIT_FILE, &json!({"path": "src/lib.rs"})));
}

#[test]
fn verification_snapshot_bundle_round_trips_without_drift() {
    let tracker = LoopTracker::with_verification_snapshot((true, FAILED_VERIFICATION_FIX_ALLOWANCE));
    assert_eq!(tracker.verification_snapshot(), (true, FAILED_VERIFICATION_FIX_ALLOWANCE));
    let cleared = LoopTracker::with_verification_snapshot((false, FAILED_VERIFICATION_FIX_ALLOWANCE));
    assert_eq!(cleared.verification_snapshot(), (false, 0));
}
