//! Task-tracker replacement, status-colour, and command-shape regressions.

use super::*;

#[test]
fn successful_task_tracker_replacement_contains_only_compact_tree_rows() {
    // Successful updates replace the prior tracker block as one compact
    // tree. Tool-call arguments are operational detail, not task-panel or
    // transcript content.
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut harness_state = build_harness_state();
    let first = serde_json::json!({
        "status": "updated",
        "checklist": {
            "items": [
                { "index_path": "1", "level": 0, "description": "Release", "status": "in_progress" },
                { "index_path": "1.1", "level": 1, "description": "Update version", "status": "completed" },
                { "index_path": "1.2", "level": 1, "description": "Run checks", "status": "in_progress" }
            ]
        }
    });
    let second = serde_json::json!({
        "status": "updated",
        "checklist": {
            "items": [
                { "index_path": "1", "level": 0, "description": "Release", "status": "completed" },
                { "index_path": "1.1", "level": 1, "description": "Update version", "status": "completed" },
                { "index_path": "1.2", "level": 1, "description": "Run checks", "status": "completed" }
            ]
        }
    });

    apply_task_tracker_block(&handle, &mut harness_state, task_tracker_block_lines(&first));
    apply_task_tracker_block(&handle, &mut harness_state, task_tracker_block_lines(&second));

    let replacement = std::iter::from_fn(|| receiver.try_recv().ok()).find_map(|command| match command {
        InlineCommand::ReplaceLast { count, lines, .. } => Some((count, lines)),
        _ => None,
    });
    let (count, rows) = replacement.expect("second tracker update should replace the previous compact tree");
    let rows = rows
        .into_iter()
        .map(|row| row.into_iter().map(|segment| segment.text).collect::<String>())
        .collect::<Vec<_>>();

    assert_eq!(count, 4);
    assert_eq!(
        rows,
        vec![
            "• Task tracker",
            "  └ Release",
            "    [x] Update version",
            "    [x] Run checks",
        ]
    );
}

// Use Tokio runtime for async test blocks
#[tokio::test]
async fn test_renderer_records_tool_and_collects_modified_files() {
    // Setup a stdout renderer
    let mut renderer = AnsiRenderer::stdout();

    // Prepare session stats and mcp state
    let mut stats = SessionStats::default();
    let mut mcp = McpPanelState::default();

    // Create an outcome that indicates write to /tmp/foo.txt
    let output_json = serde_json::json!({"result":"ok"});
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: output_json.clone(),
        stdout: None,
        modified_files: vec!["/tmp/foo.txt".to_string()],
        command_success: true,
    });

    // Invoke the shared outcome processor via a minimal output context.
    let handle = dummy_handle();
    let mut harness_state = build_harness_state();
    let mut output_ctx = OutcomeContext {
        workspace_root: None,
        session_stats: &mut stats,
        renderer: &mut renderer,
        handle: &handle,
        harness_state: &mut harness_state,
        mcp_panel_state: &mut mcp,
        vt_config: None::<&VTCodeConfig>,
    };
    let (mod_files, _last_stdout) =
        process_outcome_common(&mut output_ctx, "write_file", &serde_json::json!({}), &outcome)
            .await
            .expect("render should succeed")
            .into_tuple();

    // Confirm the function recorded the tool call
    let recorded = stats.sorted_tools();
    assert!(recorded.contains(&"write_file".to_string()));

    // Confirm the modified files list contains our path
    assert_eq!(mod_files, vec![PathBuf::from("/tmp/foo.txt")]);
}

#[test]
fn tool_call_visual_status_colours_success_failure_and_warning() {
    let palette = ColourPalette::default();
    assert_eq!(ToolDisplayStatus::Success.colour(palette), palette.success);
    assert_eq!(ToolDisplayStatus::Failure.colour(palette), palette.error);
    assert_eq!(ToolDisplayStatus::Warning.colour(palette), palette.warning);

    assert!(matches!(
        ToolDisplayStatus::from_command_output(&serde_json::json!({}), true),
        ToolDisplayStatus::Success
    ));
    assert!(matches!(
        ToolDisplayStatus::from_command_output(&serde_json::json!({}), false),
        ToolDisplayStatus::Failure
    ));
    assert!(matches!(
        ToolDisplayStatus::from_command_output(&serde_json::json!({"warning": "no results"}), true),
        ToolDisplayStatus::Warning
    ));
    assert!(matches!(
        ToolDisplayStatus::from_command_output(&serde_json::json!({"warning": null}), true),
        ToolDisplayStatus::Success
    ));

    assert!(compact_run_completion_line(&serde_json::json!({"exit_code": 0}), ToolDisplayStatus::Success).is_some());
    assert!(
        compact_run_completion_line(&serde_json::json!({"warning": "no results"}), ToolDisplayStatus::Warning)
            .is_some()
    );
    assert!(compact_run_completion_line(&serde_json::json!({}), ToolDisplayStatus::Success).is_none());
}

#[test]
fn compact_hidden_line_count_excludes_distinct_stderr() {
    let output = serde_json::json!({
        "output": "stdout line\nstderr line",
        "stdout": "stdout line",
        "stderr": "stderr line"
    });

    assert_eq!(compact_hidden_line_count(&output, None), 1);
}

#[test]
fn command_extraction_uses_canonical_command_text_shapes() {
    assert_eq!(
        extract_command_line(&serde_json::json!({"command": ["git", "status", "--short"]})),
        Some("git status --short".to_string())
    );
    assert_eq!(
        extract_command_line(&serde_json::json!({"command.0": "git", "command.1": "status"})),
        Some("git status".to_string())
    );
    assert_eq!(
        command_output_header(tools::EXECUTE_CODE, &serde_json::json!({"command": ["git", "status", "--short"]}), None),
        "• Ran git status --short"
    );
}
