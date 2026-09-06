//! Warning, file-diff, and artefact grouping-boundary regressions.

use super::*;

#[tokio::test]
async fn compact_warning_remains_visible_and_flushes_command_group() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    renderer
        .render_compact_command_activity("printf first", 0, None, None)
        .expect("seed compact command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf warning"}),
        &serde_json::json!({"warning": "no results"}),
        true,
        None,
        None,
    )
    .await
    .expect("warning command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf second"}),
        &serde_json::json!({"stdout": "second output"}),
        true,
        None,
        None,
    )
    .await
    .expect("following command should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    let activities = commands
        .iter()
        .filter_map(|command| match command {
            InlineCommand::AppendCompactActivity(activity) | InlineCommand::ReplaceCompactActivity(activity) => {
                Some(activity)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
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

    assert_eq!(activities.len(), 2);
    assert!(activities.iter().all(|activity| activity.command_count == 1));
    assert!(visible_text.contains("no results"));
}

#[tokio::test]
async fn compact_warning_flushes_non_pty_command_alias_group() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    renderer
        .render_compact_command_activity("printf first", 0, None, None)
        .expect("seed compact command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        "bash",
        &serde_json::json!({"command": "printf warning"}),
        &serde_json::json!({"warning": "no results"}),
        true,
        None,
        None,
    )
    .await
    .expect("non-PTY warning command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        "bash",
        &serde_json::json!({"command": "printf second"}),
        &serde_json::json!({"stdout": "second output"}),
        true,
        None,
        None,
    )
    .await
    .expect("following command should render");

    let activities = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|command| match command {
            InlineCommand::AppendCompactActivity(activity) | InlineCommand::ReplaceCompactActivity(activity) => {
                Some(activity)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 2);
    assert!(activities.iter().all(|activity| activity.command_count == 1));
}

#[tokio::test]
async fn compact_file_diff_is_a_glance_boundary_between_command_groups() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf first"}),
        &serde_json::json!({"stdout": "first output"}),
        true,
        None,
        None,
    )
    .await
    .expect("first command should render");

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EDIT_FILE,
        &serde_json::json!({"path": "src/lib.rs"}),
        &serde_json::json!({
            "success": true,
            "diff": [{
                "path": "src/lib.rs",
                "operation": "updated",
                "content": "@@ -1 +1 @@\n-before\n+after\n",
                "additions": 1,
                "deletions": 1,
                "truncated": false,
                "skipped": false
            }]
        }),
        true,
        None,
        None,
    )
    .await
    .expect("file diff should render");

    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf second"}),
        &serde_json::json!({"stdout": "second output"}),
        true,
        None,
        None,
    )
    .await
    .expect("second command should render");

    let commands = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();
    let activities = commands
        .iter()
        .filter_map(|command| match command {
            InlineCommand::AppendCompactActivity(activity) | InlineCommand::ReplaceCompactActivity(activity) => {
                Some(activity)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
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

    assert_eq!(activities.len(), 2);
    assert!(activities.iter().all(|activity| activity.command_count == 1));
    assert!(
        !commands
            .iter()
            .any(|command| matches!(command, InlineCommand::ReplaceCompactActivity(_)))
    );
    assert!(!visible_text.contains("Edit file"));
    assert!(visible_text.contains("• Edited src/lib.rs (+1 -1)"));
    assert!(visible_text.contains("-    1 │ before"));
    assert!(visible_text.contains("+    1 │ after"));
}

#[tokio::test]
async fn compact_command_artefacts_start_a_fresh_group() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    renderer
        .render_compact_command_activity("printf first", 1, None, None)
        .expect("seed compact command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf second"}),
        &serde_json::json!({"stdout": "normal output", "stderr": "diagnostic output"}),
        true,
        None,
        None,
    )
    .await
    .expect("artefact-bearing command should render");

    let activities = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|command| match command {
            InlineCommand::AppendCompactActivity(activity) | InlineCommand::ReplaceCompactActivity(activity) => {
                Some(activity)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 2);
    assert!(activities.iter().all(|activity| activity.command_count == 1));
}

#[tokio::test]
async fn compact_pty_artefacts_flush_a_preceding_command_group() {
    let (sender, mut receiver) = unbounded_channel();
    let handle = InlineHandle::new_for_tests(sender);
    let mut renderer = AnsiRenderer::with_inline_ui(handle.clone(), Default::default());

    renderer
        .render_compact_command_activity("printf first", 0, None, None)
        .expect("seed compact command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "printf diagnostic"}),
        &serde_json::json!({"output": "normal output", "stderr": "diagnostic output"}),
        true,
        None,
        None,
    )
    .await
    .expect("artefact-bearing PTY command should render");
    render_tool_output_common(
        &mut renderer,
        &handle,
        tools::EXECUTE_CODE,
        &serde_json::json!({"command": "printf second"}),
        &serde_json::json!({"stdout": "normal output"}),
        true,
        None,
        None,
    )
    .await
    .expect("following command should render");

    let activities = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|command| match command {
            InlineCommand::AppendCompactActivity(activity) | InlineCommand::ReplaceCompactActivity(activity) => {
                Some(activity)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(activities.len(), 2);
    assert!(activities.iter().all(|activity| activity.command_count == 1));
}
