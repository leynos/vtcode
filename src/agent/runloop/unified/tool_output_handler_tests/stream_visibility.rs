//! Distinct stream visibility and PTY input-forwarding regressions.

use super::*;

#[tokio::test]
async fn compact_command_output_keeps_distinct_stderr_visible() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({
        "stdout": "normal output",
        "stderr": "diagnostic output"
    });

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf test"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("stderr-bearing command output should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
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

    assert!(
        commands
            .iter()
            .any(|command| matches!(command, InlineCommand::AppendCompactActivity(_)))
    );
    assert!(visible_text.contains("diagnostic output"));
    assert!(!visible_text.contains("normal output"));
}

#[tokio::test]
async fn compact_command_output_preserves_identical_stdout_and_stderr() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({
        "stdout": "same output",
        "stderr": "same output"
    });

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf test"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("identical named streams should remain visible");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
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
    assert!(visible_text.contains("same output"));
    let capture_text = commands
        .iter()
        .find_map(|command| match command {
            InlineCommand::RecordToolOutput { lines, .. } => Some(lines.join("\n")),
            _ => None,
        })
        .expect("complete command capture should be retained");
    assert!(capture_text.contains("stdout"));
    assert!(capture_text.contains("stderr"));
    assert_eq!(capture_text.matches("same output").count(), 2);
}

#[tokio::test]
async fn pty_input_forwarding_does_not_collapse_as_a_command() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::SEND_PTY_INPUT,
        &serde_json::json!({"session_id": "pty-1", "chars": ""}),
        &serde_json::json!({"output": "polled output"}),
        true,
        None,
        None,
    )
    .await
    .expect("PTY input forwarding should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    assert!(!commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendCompactActivity(_) | InlineCommand::CollapsePtyBlock(_))
    }));
    assert!(!commands.iter().any(|command| {
        matches!(command, InlineCommand::AppendLine { segments, .. }
            if segments.iter().any(|segment| segment.text.contains("• Ran send_pty_input")))
    }));
}

#[tokio::test]
async fn compact_pty_output_keeps_distinct_stderr_visible() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());
    let output = serde_json::json!({
        "output": "terminal output",
        "stderr": "pty diagnostic",
        "is_exited": true,
        "exit_code": 0
    });

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "printf test"}),
        &output,
        true,
        None,
        None,
    )
    .await
    .expect("PTY stderr should render");

    let visible_text = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|command| match command {
            InlineCommand::AppendLine { segments, .. } => {
                Some(segments.into_iter().map(|segment| segment.text).collect::<String>())
            }
            InlineCommand::Inline { segment, .. } => Some(segment.text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");

    assert!(visible_text.contains("stderr: pty diagnostic"), "visible text: {visible_text:?}");
    assert!(!visible_text.contains("• Ran printf test ·"));
}
