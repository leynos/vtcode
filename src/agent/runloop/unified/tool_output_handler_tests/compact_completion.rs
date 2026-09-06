//! Compact PTY and command completion rendering regressions.

use super::*;

#[tokio::test]
async fn compact_pty_completion_emits_grouped_activity_without_live_preview() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "printf first"}),
        &serde_json::json!({"stdout": "first\nsecond"}),
        true,
        None,
        None,
    )
    .await
    .expect("compact PTY output should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        commands
            .iter()
            .any(|command| matches!(command, InlineCommand::CollapsePtyBlock(_)))
    );
    assert!(
        !commands.iter().any(|command| {
            matches!(
                command,
                InlineCommand::AppendLine { .. } | InlineCommand::Inline { .. } | InlineCommand::ReplaceLast { .. }
            )
        }),
        "compact PTY completion must not flash a live output block"
    );
}

#[tokio::test]
async fn compact_pty_attention_keeps_command_summary_and_stderr_visible() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "printf diagnostic"}),
        &serde_json::json!({"stdout": "normal output", "stderr": "diagnostic output"}),
        true,
        None,
        None,
    )
    .await
    .expect("compact PTY diagnostics should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendToolOutputLine { segments, .. }
            if segments.iter().map(|segment| segment.text.as_str()).collect::<String>().contains("• Ran printf diagnostic"))
    }));
    assert!(commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendLine { segments, .. }
            if segments.iter().map(|segment| segment.text.as_str()).collect::<String>().contains("stderr: diagnostic output"))
    }));
}

#[test]
fn compact_pty_failure_keeps_command_summary_without_live_preview() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let mut stats = SessionStats::default();
    let mut mcp = McpPanelState::default();
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
    let status = ToolExecutionStatus::Failure {
        error: ToolExecutionError::from_anyhow(
            tools::RUN_PTY_CMD,
            &anyhow::anyhow!("command failed"),
            0,
            false,
            false,
            Some("test"),
        ),
    };

    handle_non_success_common(&mut output_ctx, tools::RUN_PTY_CMD, &serde_json::json!({"command": "false"}), &status)
        .expect("compact PTY failure should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendToolOutputLine { segments, .. }
            if segments.iter().map(|segment| segment.text.as_str()).collect::<String>().contains("• Ran false"))
    }));
}

#[tokio::test]
async fn compact_command_output_emits_group_metadata_and_complete_capture() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({"stdout": "first\nsecond"});

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf first"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("compact command output should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    let capture_id = commands.iter().find_map(|command| match command {
        InlineCommand::RecordToolOutput { id, .. } => Some(*id),
        _ => None,
    });
    let activity = commands.iter().find_map(|command| match command {
        InlineCommand::AppendCompactActivity(activity) => Some(activity),
        _ => None,
    });

    let capture_id = capture_id.expect("complete command capture should be retained");
    let activity = activity.expect("compact command activity should be emitted");
    assert_eq!(activity.review_anchor, Some(capture_id));
    assert_eq!(activity.hidden_line_count, 2);
    assert_eq!(activity.display_text(), "• Ran printf first · … +2 lines");
    assert!(
        commands
            .iter()
            .all(|command| { !matches!(command, InlineCommand::AppendLine { .. } | InlineCommand::Inline { .. }) })
    );
}

#[tokio::test]
async fn compact_command_capture_keeps_follow_up_guidance() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({
        "stdout": "command output",
        "next_action": "Review the result before continuing."
    });

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf guidance"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("guidance-bearing command output should render");

    let capture = std::iter::from_fn(|| receiver.try_recv().ok()).find_map(|command| match command {
        InlineCommand::RecordToolOutput { lines, .. } => Some(lines),
        _ => None,
    });
    let capture = capture.expect("complete command capture should be retained");
    assert!(capture.iter().any(|line| line.contains("Review the result before continuing.")));
}

#[tokio::test]
async fn compact_command_capture_keeps_structured_result_metadata() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({
        "stdout": "command output",
        "generated_files": {
            "count": 1,
            "files": ["src/generated.rs"],
            "summary": "Generated one file"
        },
        "metadata_flag": false,
        "metadata_count": 0,
        "fallback_tool": tools::CODE_SEARCH,
        "fallback_tool_args": {"query": "generated"},
        "stderr_preview": "no stderr was emitted"
    });

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "generate"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("structured command output should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    let capture = commands.iter().find_map(|command| match command {
        InlineCommand::RecordToolOutput { lines, .. } => Some(lines.join("\n")),
        _ => None,
    });
    let capture = capture.expect("complete command capture should be retained");
    assert!(capture.contains("structured output"));
    assert!(capture.contains("generated_files"));
    assert!(capture.contains("src/generated.rs"));
    assert!(capture.contains("metadata_flag"));
    assert!(capture.contains("metadata_count"));
    assert!(capture.contains("fallback_tool"));
    assert!(capture.contains("fallback_tool_args"));
    assert!(capture.contains("stderr_preview"));

    let visible_text = commands
        .iter()
        .filter_map(|command| match command {
            InlineCommand::AppendLine { segments, .. } => {
                Some(segments.iter().map(|segment| segment.text.as_str()).collect::<String>())
            }
            InlineCommand::Inline { segment, .. } => Some(segment.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(visible_text.contains("generated_files"));
    assert!(visible_text.contains("src/generated.rs"));
}

#[tokio::test]
async fn expanded_command_summary_carries_capture_identity() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    renderer.set_tool_display_mode(ToolDisplayMode::Expanded);

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf identity"}),
        &serde_json::json!({"stdout": "captured output"}),
        true,
        None,
        None,
    )
    .await
    .expect("expanded command output should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    let capture_id = commands.iter().find_map(|command| match command {
        InlineCommand::RecordToolOutput { id, .. } => Some(*id),
        _ => None,
    });
    let summary_id = commands.iter().find_map(|command| match command {
        InlineCommand::AppendToolOutputLine { id, .. } => Some(*id),
        _ => None,
    });

    assert_eq!(summary_id, capture_id);
}
