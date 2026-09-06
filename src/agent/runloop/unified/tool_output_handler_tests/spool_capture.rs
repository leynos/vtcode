//! Workspace spool loading, transcript, and complete-viewer regressions.

use super::*;

#[tokio::test]
async fn pty_capture_reads_complete_workspace_spool() {
    let workspace = TempDir::new().expect("workspace temp dir");
    let spool_path = workspace.path().join(".vtcode/context/tool_outputs/pty.txt");
    tokio::fs::create_dir_all(spool_path.parent().expect("spool parent"))
        .await
        .expect("create spool parent");
    tokio::fs::write(&spool_path, "first complete line\nsecond complete line\n")
        .await
        .expect("write spool");

    let output = serde_json::json!({
        "spool_path": ".vtcode/context/tool_outputs/pty.txt",
        "output": "first preview line"
    });

    assert_eq!(
        load_complete_output(&output, Some(workspace.path())).await.as_deref(),
        Some("first complete line\nsecond complete line\n")
    );
}

#[tokio::test]
async fn pty_capture_rejects_spool_outside_workspace() {
    let workspace = TempDir::new().expect("workspace temp dir");
    let outside = TempDir::new().expect("outside temp dir");
    let spool_path = outside.path().join("pty.txt");
    tokio::fs::write(&spool_path, "secret outside workspace")
        .await
        .expect("write outside spool");

    let output = serde_json::json!({ "spool_path": spool_path });

    assert!(load_complete_output(&output, Some(workspace.path())).await.is_none());
}

#[tokio::test]
async fn pty_capture_rejects_malformed_spool_metadata_without_inline_fallback() {
    let workspace = TempDir::new().expect("workspace temp dir");
    let output = serde_json::json!({
        "spool_path": null,
        "output": "untrusted inline fallback"
    });

    assert!(load_complete_output(&output, Some(workspace.path())).await.is_none());
}

#[tokio::test]
async fn test_renderer_records_mcp_event_for_mcp_tool() {
    let mut renderer = AnsiRenderer::stdout();

    // Note: tests involving `apply_turn_outcome` live in `turn/turn_loop.rs` and can be added there
    let mut stats = SessionStats::default();
    let mut mcp = McpPanelState::new(32, true); // enabled

    let output_json = serde_json::json!({"exit_code":0});
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: output_json.clone(),
        stdout: Some("ok".to_string()),
        modified_files: vec![],
        command_success: true,
    });

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
    let (_mod_files, _last_stdout) =
        process_outcome_common(&mut output_ctx, "mcp_example", &serde_json::json!({}), &outcome)
            .await
            .expect("render should succeed")
            .into_tuple();

    // Ensure mcp panel recorded an event
    assert!(mcp.event_count() > 0);
}

#[tokio::test]
async fn spooled_exec_output_keeps_transcript_at_reference_only() {
    let mut renderer = AnsiRenderer::stdout();
    let mut stats = SessionStats::default();
    let mut mcp = McpPanelState::default();
    let handle = dummy_handle();
    let mut harness_state = build_harness_state();

    transcript::clear();

    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "output": "preview text that should stay out of transcript persistence",
            "spool_path": ".vtcode/context/tool_outputs/exec_command_1.txt",
            "exit_code": 0,
            "is_exited": true
        }),
        stdout: Some("preview text that should stay out of transcript persistence".to_string()),
        modified_files: vec![],
        command_success: true,
    });

    let mut output_ctx = OutcomeContext {
        workspace_root: None,
        session_stats: &mut stats,
        renderer: &mut renderer,
        handle: &handle,
        harness_state: &mut harness_state,
        mcp_panel_state: &mut mcp,
        vt_config: None::<&VTCodeConfig>,
    };

    process_outcome_common(
        &mut output_ctx,
        tools::UNIFIED_EXEC,
        &serde_json::json!({
            "action": "run",
            "command": "cargo check -p vtcode-core"
        }),
        &outcome,
    )
    .await
    .expect("render should succeed");

    let transcript_lines = transcript::snapshot();
    let transcript_text = transcript_lines.join("\n");
    let stripped_text = vtcode_core::utils::ansi_parser::strip_ansi(&transcript_text);
    assert!(stripped_text.contains("Large output was spooled to"), "Transcript: {stripped_text:?}");
    assert!(!stripped_text.contains("preview text that should stay out of transcript persistence"));

    transcript::clear();
}

#[tokio::test]
async fn inline_tool_output_viewer_retains_complete_spooled_capture() {
    let workspace = TempDir::new().expect("workspace temp dir");
    let spool_path = workspace.path().join(".vtcode/context/tool_outputs/exec_command_1.txt");
    tokio::fs::create_dir_all(spool_path.parent().expect("spool parent"))
        .await
        .expect("create spool parent");
    tokio::fs::write(&spool_path, "first complete line\nsecond complete line\n")
        .await
        .expect("write spool");

    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let mut stats = SessionStats::default();
    let mut mcp = McpPanelState::default();
    let mut harness_state = build_harness_state();
    transcript::clear();
    let outcome = ToolPipelineOutcome::from_status(ToolExecutionStatus::Success {
        output: serde_json::json!({
            "output": "preview line",
            "spool_path": ".vtcode/context/tool_outputs/exec_command_1.txt",
            "exit_code": 0,
            "is_exited": true
        }),
        stdout: Some("preview line".to_string()),
        modified_files: vec![],
        command_success: true,
    });
    let mut output_ctx = OutcomeContext {
        workspace_root: Some(workspace.path()),
        session_stats: &mut stats,
        renderer: &mut renderer,
        handle: &handle,
        harness_state: &mut harness_state,
        mcp_panel_state: &mut mcp,
        vt_config: None::<&VTCodeConfig>,
    };

    process_outcome_common(
        &mut output_ctx,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "cargo check"}),
        &outcome,
    )
    .await
    .expect("render should succeed");

    let mut recorded = None;
    let mut commands = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        if let InlineCommand::RecordToolOutput { lines, .. } = &command {
            recorded = Some(lines.clone());
        }
        commands.push(command);
    }
    let lines = recorded.expect("the complete output should be recorded for the viewer");
    assert_eq!(lines[0], "• Ran cargo check");
    assert!(lines.iter().any(|line| line == "  └ first complete line"));
    assert!(lines.iter().any(|line| line == "    second complete line"));
    assert!(!lines.iter().any(|line| line.contains("preview line")));
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, InlineCommand::CollapsePtyBlock(_))),
        "received {} inline commands",
        commands.len()
    );

    let transcript_text = transcript::snapshot().join("\n");
    assert!(!transcript_text.contains("first complete line"));
    assert!(!transcript_text.contains("second complete line"));
    transcript::clear();
}

#[tokio::test]
async fn unavailable_spool_capture_remains_visible_and_does_not_collapse_pty() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "printf preview"}),
        &serde_json::json!({"spool_path": null, "output": "preview output"}),
        true,
        None,
        None,
    )
    .await
    .expect("unavailable spool result should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendLine { segments, .. }
            if segments.iter().any(|segment| segment.text.contains("capture unavailable")))
    }));
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, InlineCommand::CollapsePtyBlock(_)))
    );
}
