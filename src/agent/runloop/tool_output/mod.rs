mod commands;
mod commands_processing;
mod files;
pub(crate) mod large_output;
#[cfg(test)]
mod large_output_tests;
mod mcp;
mod panels;
mod streams;
mod styles;

// Re-export stream utilities
use anyhow::Result;
use commands::render_terminal_command_panel;
use files::{
    format_diff_content_lines_with_numbers, render_apply_patch_diff_preview, render_list_dir_output,
    render_read_file_output, render_write_file_preview,
};
use mcp::{render_context7_output, render_generic_output, render_sequential_output, resolve_renderer_profile};
use serde_json::Value;
use streams::render_stream_section;
pub(crate) use streams::{render_code_fence_blocks, resolve_stdout_tail_limit};
use styles::{GitStyles, LsStyles};
use vtcode_core::config::ToolOutputMode;
use vtcode_core::config::constants::tools;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::config::mcp::McpRendererProfile;
use vtcode_core::tools::continuation::{
    NEXT_CONTINUE_PROMPT, NEXT_READ_PROMPT, PtyContinuationArgs, ReadChunkContinuationArgs,
};
use vtcode_core::tools::handlers::task_tracking::compact_task_tree_view_from_items;
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};
use vtcode_core::utils::style_helpers::{ColourPalette, render_styled};
use vtcode_ui::tui::app::TaskPanelMetadata;

pub(crate) fn spooled_output_hint(path: &str) -> String {
    format!(
        "Large output was spooled to \"{path}\". Use exec_command with shell tools such as cat, sed, or rg to inspect details."
    )
}

/// Render a detail line with tree prefix styling (└ prefix).
/// Used to unify output details with the tree structure used by other tools.
pub(crate) fn render_tree_detail(renderer: &mut AnsiRenderer, detail: &str) -> Result<()> {
    let palette = ColourPalette::default();
    let mut styled = String::new();
    push_tree_prefix(&mut styled, &palette);
    styled.push_str(&render_styled(detail, palette.muted, None));
    renderer.line(MessageStyle::Info, &styled)?;
    Ok(())
}

/// Push the shared `  └ ` tree-detail prefix (two-space indent, dim `└`, trailing
/// space) into `styled`. Centralized so the prefix style can change in one place
/// across both detail lines and command-line renderings.
pub(crate) fn push_tree_prefix(styled: &mut String, palette: &ColourPalette) {
    styled.push_str("  ");
    styled.push_str(&render_styled("└", palette.muted, Some("dim".to_string())));
    styled.push(' ');
}

fn tool_recovery_hint(val: &Value) -> Option<&'static str> {
    if !val.get("loop_detected").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    if val.get("spool_path").and_then(Value::as_str).is_some() {
        return Some("Loop detected; continue from spooled output.");
    }
    if val.get("fallback_tool").and_then(Value::as_str).is_some() {
        return Some("Loop detected; fallback is available.");
    }
    Some("Loop detected; change approach before retrying.")
}

fn push_tool_follow_up_hint(hints: &mut Vec<String>, hint: impl Into<String>) {
    let hint = hint.into();
    if hint.trim().is_empty() || hints.iter().any(|existing| existing == &hint) {
        return;
    }
    hints.push(hint);
}

fn tool_follow_up_hints(val: &Value) -> Vec<String> {
    let mut hints = Vec::with_capacity(5);
    if let Some(hint) = tool_recovery_hint(val) {
        push_tool_follow_up_hint(&mut hints, hint);
    }
    if let Some(next_action) = val.get("next_action").and_then(Value::as_str) {
        push_tool_follow_up_hint(&mut hints, next_action);
    }
    if let Some(path) = val.get("spool_path").and_then(Value::as_str) {
        push_tool_follow_up_hint(&mut hints, spooled_output_hint(path));
    }
    if val
        .get("next_continue_args")
        .and_then(PtyContinuationArgs::from_value)
        .is_some()
    {
        push_tool_follow_up_hint(&mut hints, NEXT_CONTINUE_PROMPT);
    } else if val
        .get("next_read_args")
        .and_then(ReadChunkContinuationArgs::from_value)
        .is_some()
    {
        push_tool_follow_up_hint(&mut hints, NEXT_READ_PROMPT);
    }
    hints
}

/// Return follow-up guidance in the same order used by the live renderer.
///
/// The transcript viewer stores complete command captures separately from the
/// compact live row. Keep this metadata in that capture so exporting or
/// reviewing the whole conversation does not lose recovery instructions that
/// were rendered after the command body.
pub(crate) fn tool_follow_up_hints_for_capture(val: &Value, rendered_output: Option<&str>) -> Vec<String> {
    tool_follow_up_hints(val)
        .into_iter()
        .filter(|hint| !rendered_output.is_some_and(|output| output.contains(hint.as_str())))
        .collect()
}

pub(super) fn render_tool_follow_up_hints(
    renderer: &mut AnsiRenderer,
    val: &Value,
    rendered_output: Option<&str>,
) -> Result<()> {
    let mut rendered_any = false;
    for hint in tool_follow_up_hints(val) {
        if rendered_output.is_some_and(|output| output.contains(hint.as_str())) {
            continue;
        }
        if !rendered_any {
            renderer.line(MessageStyle::ToolDetail, "")?;
            rendered_any = true;
        }
        renderer.line(MessageStyle::ToolDetail, &hint)?;
    }
    Ok(())
}

fn preferred_follow_up_rendered_body(val: &Value) -> Option<&str> {
    val.get("output")
        .and_then(Value::as_str)
        .or_else(|| val.get("content").and_then(Value::as_str))
}

fn render_tool_follow_up_hints_for_value(renderer: &mut AnsiRenderer, val: &Value) -> Result<()> {
    render_tool_follow_up_hints(renderer, val, preferred_follow_up_rendered_body(val))
}

async fn render_terminal_tool_output(
    renderer: &mut AnsiRenderer,
    val: &Value,
    vt_config: Option<&VTCodeConfig>,
    allow_tool_ansi: bool,
) -> Result<()> {
    let git_styles = GitStyles::new();
    let ls_styles = LsStyles::from_env();
    render_terminal_command_panel(renderer, val, &git_styles, &ls_styles, vt_config, allow_tool_ansi).await
}

pub(crate) async fn render_tool_output(
    renderer: &mut AnsiRenderer,
    tool_name: Option<&str>,
    val: &Value,
    vt_config: Option<&VTCodeConfig>,
) -> Result<()> {
    let allow_tool_ansi = vt_config.map(|cfg| cfg.ui.allow_tool_ansi).unwrap_or(false);
    let is_git_diff_output = is_git_diff_payload(val);

    match tool_name {
        Some(tools::WRITE_FILE) | Some(tools::CREATE_FILE) => {
            let git_styles = GitStyles::new();
            let ls_styles = LsStyles::from_env();
            return render_write_file_preview(renderer, val, &git_styles, &ls_styles);
        }
        Some(tools::EDIT_FILE) | Some(tools::SEARCH_REPLACE) | Some(tools::DELETE_FILE)
            if val.get("diff").is_some() || val.get("diff_preview").is_some() =>
        {
            let git_styles = GitStyles::new();
            let ls_styles = LsStyles::from_env();
            return render_write_file_preview(renderer, val, &git_styles, &ls_styles);
        }
        Some(tools::APPLY_PATCH) => {
            let git_styles = GitStyles::new();
            let ls_styles = LsStyles::from_env();
            return render_apply_patch_diff_preview(renderer, val, &git_styles, &ls_styles);
        }
        Some(tools::UNIFIED_FILE) => {
            if val.get("diff").is_some() || val.get("diff_preview").is_some() {
                let git_styles = GitStyles::new();
                let ls_styles = LsStyles::from_env();
                return render_write_file_preview(renderer, val, &git_styles, &ls_styles);
            }
            if val.get("content").is_some() {
                render_read_file_output(renderer, val)?;
                render_tool_follow_up_hints(renderer, val, val.get("content").and_then(Value::as_str))?;
                return Ok(());
            }
        }
        Some(tools::RUN_PTY_CMD)
        | Some(tools::READ_PTY_SESSION)
        | Some(tools::CREATE_PTY_SESSION)
        | Some(tools::SEND_PTY_INPUT)
        | Some(tools::CLOSE_PTY_SESSION)
        | Some(tools::RESIZE_PTY_SESSION)
        | Some(tools::LIST_PTY_SESSIONS)
        | Some(tools::EXEC_COMMAND)
        | Some(tools::EXEC_PTY_CMD) => {
            return render_terminal_tool_output(renderer, val, vt_config, allow_tool_ansi).await;
        }
        Some(tools::UNIFIED_EXEC) if !is_git_diff_output && should_render_command_session_terminal_panel(val) => {
            return render_terminal_tool_output(renderer, val, vt_config, allow_tool_ansi).await;
        }
        Some(tools::WEB_FETCH) => {
            render_generic_output(renderer, val)?;
            render_tool_follow_up_hints_for_value(renderer, val)?;
            return Ok(());
        }
        Some(tools::LIST_FILES) => {
            let ls_styles = LsStyles::from_env();
            render_list_dir_output(renderer, val, &ls_styles)?;
            render_tool_follow_up_hints_for_value(renderer, val)?;
            return Ok(());
        }
        Some(tools::READ_FILE) => {
            render_read_file_output(renderer, val)?;
            render_tool_follow_up_hints(renderer, val, val.get("content").and_then(Value::as_str))?;
            return Ok(());
        }
        Some(tools::EXECUTE_CODE) => {
            return render_terminal_tool_output(renderer, val, vt_config, allow_tool_ansi).await;
        }
        Some(tools::TASK_TRACKER) => {
            if render_tracker_view(renderer, val)? {
                return Ok(());
            }
        }
        _ => {}
    }

    render_simple_tool_status(renderer, tool_name, val)?;

    if let Some(notice) = val.get("security_notice").and_then(Value::as_str) {
        renderer.line(MessageStyle::ToolDetail, notice)?;
    }

    render_tool_follow_up_hints_for_value(renderer, val)?;

    if let Some(tool) = tool_name
        && tool.starts_with("mcp_")
    {
        if let Some(profile) = resolve_renderer_profile(tool, vt_config) {
            match profile {
                McpRendererProfile::Context7 => render_context7_output(renderer, val)?,
                McpRendererProfile::SequentialThinking => render_sequential_output(renderer, val)?,
            }
        } else {
            render_generic_output(renderer, val)?;
        }
        // Early return for MCP tools - don't fall through to other rendering logic
        return Ok(());
    }

    let output_mode = vt_config.map(|cfg| cfg.ui.tool_output_mode).unwrap_or(ToolOutputMode::Compact);
    let tail_limit = resolve_stdout_tail_limit(vt_config);
    let git_styles = GitStyles::new();
    let ls_styles = LsStyles::from_env();
    let disable_spool = val.get("no_spool").and_then(Value::as_bool).unwrap_or(false);

    // PTY tools use "output" field instead of "stdout"
    let stream_tool_name = if is_git_diff_output { None } else { tool_name };

    if let Some(output) = val.get("output").and_then(Value::as_str) {
        render_stream_section(
            renderer,
            "",
            output,
            output_mode,
            tail_limit,
            stream_tool_name,
            &git_styles,
            &ls_styles,
            MessageStyle::ToolOutput,
            allow_tool_ansi,
            disable_spool,
            vt_config,
        )
        .await?;
    } else if let Some(stdout) = val.get("stdout").and_then(Value::as_str) {
        render_stream_section(
            renderer,
            "stdout",
            stdout,
            output_mode,
            tail_limit,
            stream_tool_name,
            &git_styles,
            &ls_styles,
            MessageStyle::ToolOutput,
            allow_tool_ansi,
            disable_spool,
            vt_config,
        )
        .await?;
    }
    if let Some(stderr) = val.get("stderr").and_then(Value::as_str) {
        render_stream_section(
            renderer,
            "stderr",
            stderr,
            output_mode,
            tail_limit,
            tool_name,
            &git_styles,
            &ls_styles,
            MessageStyle::ToolError,
            allow_tool_ansi,
            disable_spool,
            vt_config,
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn format_unified_diff_lines(diff_content: &str) -> Vec<String> {
    format_diff_content_lines_with_numbers(diff_content)
}

fn is_git_diff_payload(val: &Value) -> bool {
    val.get("content_type")
        .and_then(Value::as_str)
        .is_some_and(|content_type| content_type == "git_diff")
}

pub(crate) fn tracker_view_lines(val: &Value) -> Vec<String> {
    let view = val.get("view").and_then(Value::as_object);
    let checklist_items = val
        .get("checklist")
        .and_then(Value::as_object)
        .and_then(|checklist| checklist.get("items"))
        .and_then(Value::as_array);
    let compact_rows = checklist_items
        .filter(|items| !items.is_empty())
        .map(|items| compact_task_tree_view_from_items(items))
        .unwrap_or_default();
    let view_rows = if compact_rows.is_empty() {
        view.and_then(|obj| obj.get("lines"))
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(visible_tracker_view_row).collect::<Vec<_>>())
            .unwrap_or_default()
    } else {
        compact_rows.iter().filter_map(visible_tracker_view_row).collect::<Vec<_>>()
    };
    let summary_lines = tracker_summary_lines(val);

    if view_rows.is_empty() && summary_lines.is_empty() {
        return Vec::new();
    }

    let title = view
        .and_then(|obj| obj.get("title"))
        .and_then(Value::as_str)
        .or_else(|| val.get("checklist").and_then(|c| c.get("title")).and_then(Value::as_str))
        .unwrap_or("Task tracker");

    let mut lines = Vec::with_capacity(view_rows.len() + summary_lines.len() + 1);
    lines.push(format!("• {title}"));
    lines.extend(summary_lines);
    lines.extend(view_rows);
    lines
}

fn visible_tracker_view_row(value: &Value) -> Option<String> {
    let display = value.get("display").and_then(Value::as_str).or_else(|| value.as_str())?;
    let trimmed = display.trim_start();
    if trimmed.starts_with("files:") || trimmed.starts_with("outcome:") || trimmed.starts_with("verify:") {
        return None;
    }
    Some(display.to_string())
}

pub(crate) fn tracker_panel_metadata(val: &Value) -> Option<TaskPanelMetadata> {
    let checklist = val.get("checklist").and_then(Value::as_object)?;
    let title = checklist
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())?
        .to_string();
    let completed = usize::try_from(checklist.get("completed").and_then(Value::as_u64)?).ok()?;
    let total = usize::try_from(checklist.get("total").and_then(Value::as_u64)?).ok()?;
    Some(TaskPanelMetadata { title, completed, total })
}

fn render_tracker_view(renderer: &mut AnsiRenderer, val: &Value) -> Result<bool> {
    let lines = tracker_view_lines(val);
    if lines.is_empty() {
        return Ok(false);
    }

    for line in lines {
        renderer.line(MessageStyle::ToolDetail, &line)?;
    }

    Ok(true)
}

fn tracker_summary_lines(val: &Value) -> Vec<String> {
    let has_valid_checklist_items = val
        .get("checklist")
        .and_then(Value::as_object)
        .and_then(|checklist| checklist.get("items"))
        .and_then(Value::as_array)
        .is_some_and(|items| !compact_task_tree_view_from_items(items).is_empty());
    if has_valid_checklist_items && tracker_response_is_successful(val) {
        return Vec::new();
    }

    tracker_diagnostic_lines(val)
}

fn tracker_response_is_successful(val: &Value) -> bool {
    if val.get("error").is_some() || val.get("error_type").is_some() {
        return false;
    }

    match val.get("status").and_then(Value::as_str) {
        Some("created" | "replaced" | "updated" | "unchanged" | "ok" | "added") => true,
        Some(_) => false,
        None => true,
    }
}

fn tracker_diagnostic_lines(val: &Value) -> Vec<String> {
    let mut lines = Vec::new();

    if let Some(status) = val.get("status").and_then(Value::as_str)
        && !status.trim().is_empty()
    {
        lines.push(format!("  Tracker status: {status}"));
    }

    if let Some(message) = val.get("message").and_then(Value::as_str)
        && !message.trim().is_empty()
    {
        lines.push(format!("  Update: {message}"));
    }

    lines
}

fn render_simple_tool_status(renderer: &mut AnsiRenderer, _tool_name: Option<&str>, val: &Value) -> Result<()> {
    let has_error = val.get("error").is_some() || val.get("error_type").is_some();

    if has_error {
        render_error_details(renderer, val)?;
    }

    Ok(())
}

fn should_render_command_session_terminal_panel(val: &Value) -> bool {
    let has_command = val
        .get("command")
        .map(|command| match command {
            Value::String(text) => !text.trim().is_empty(),
            Value::Array(parts) => !parts.is_empty(),
            _ => false,
        })
        .unwrap_or(false);
    let has_terminal_stream = val
        .get("output")
        .and_then(Value::as_str)
        .is_some_and(|text| !text.trim().is_empty())
        || val
            .get("stdout")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
        || val
            .get("stderr")
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty());
    let has_session_context = ["id", "session_id", "process_id", "is_exited", "exit_code"]
        .iter()
        .any(|key| val.get(*key).is_some());

    !is_git_diff_payload(val) && (has_command || has_terminal_stream || has_session_context)
}

fn render_error_details(renderer: &mut AnsiRenderer, val: &Value) -> Result<()> {
    if let Some(error_msg) = val
        .get("message")
        .and_then(|v| v.as_str())
        .filter(|msg| !msg.trim().is_empty())
        .or_else(|| val.get("error").and_then(|v| v.as_str()).filter(|msg| !msg.trim().is_empty()))
    {
        renderer.line(MessageStyle::ToolError, &format!("Error: {error_msg}"))?;
    }

    if let Some(error_type) = val.get("error_type").and_then(|v| v.as_str()) {
        let type_description = match error_type {
            "InvalidParameters" => "Invalid parameters provided",
            "ToolNotFound" => "Tool not found",
            "ResourceNotFound" => "Resource not found",
            "PermissionDenied" => "Permission denied",
            "ExecutionError" => "Execution error",
            "PolicyViolation" => "Policy violation",
            "Timeout" => "Operation timed out",
            "NetworkError" => "Network error",
            "EncodingError" => "Encoding error",
            "FileSystemError" => "File system error",
            _ => error_type,
        };
        renderer.line(MessageStyle::ToolDetail, &format!("Type: {type_description}"))?;
    }

    if let Some(original) = val.get("original_error").and_then(|v| v.as_str())
        && !original.trim().is_empty()
    {
        let display_error = vtcode_commons::formatting::truncate_byte_budget(original, 197, "...");
        renderer.line(MessageStyle::ToolDetail, &format!("Details: {display_error}"))?;
    }

    if let Some(path) = val.get("path").and_then(|v| v.as_str()) {
        renderer.line(MessageStyle::ToolDetail, &format!("Path: {path}"))?;
    }

    if let Some(line) = val.get("line").and_then(|v| v.as_u64()) {
        if let Some(col) = val.get("column").and_then(|v| v.as_u64()) {
            renderer.line(MessageStyle::ToolDetail, &format!("Location: line {line}, column {col}"))?;
        } else {
            renderer.line(MessageStyle::ToolDetail, &format!("Location: line {line}"))?;
        }
    }

    if let Some(suggestions) = val.get("recovery_suggestions").and_then(|v| v.as_array())
        && !suggestions.is_empty()
    {
        renderer.line(MessageStyle::ToolDetail, "")?;
        renderer.line(MessageStyle::ToolDetail, "Suggestions:")?;
        for (idx, suggestion) in suggestions.iter().take(5).enumerate() {
            if let Some(text) = suggestion.as_str() {
                renderer.line(MessageStyle::ToolDetail, &format!("{}. {}", idx + 1, text))?;
            }
        }
        if suggestions.len() > 5 {
            renderer.line(MessageStyle::ToolDetail, &format!("    ... and {} more", suggestions.len() - 5))?;
        }
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn collect_inline_output(
    receiver: &mut tokio::sync::mpsc::UnboundedReceiver<vtcode_core::ui::InlineCommand>,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    while let Ok(command) = receiver.try_recv() {
        match command {
            vtcode_core::ui::InlineCommand::AppendLine { segments, .. } => {
                lines.push(segments.into_iter().map(|segment| segment.text).collect::<String>());
            }
            vtcode_core::ui::InlineCommand::ReplaceLast { lines: replacement_lines, .. } => {
                lines.extend(
                    replacement_lines
                        .into_iter()
                        .map(|line| line.into_iter().map(|segment| segment.text).collect::<String>()),
                );
            }
            _ => {}
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vtcode_core::config::ToolDisplayMode;
    use vtcode_core::ui::InlineHandle;
    use vtcode_core::utils::ansi::AnsiRenderer;

    use super::{
        collect_inline_output, preferred_follow_up_rendered_body, render_tool_output,
        should_render_command_session_terminal_panel, spooled_output_hint, tracker_panel_metadata,
        tracker_summary_lines, tracker_view_lines,
    };

    #[test]
    fn command_session_terminal_panel_detects_command_payload() {
        let payload = json!({
            "command": "cargo check",
            "output": "Checking vtcode"
        });
        assert!(should_render_command_session_terminal_panel(&payload));
    }

    #[test]
    fn command_session_terminal_panel_detects_session_payload() {
        let payload = json!({
            "session_id": "run-123",
            "is_exited": true
        });
        assert!(should_render_command_session_terminal_panel(&payload));
    }

    #[test]
    fn command_session_terminal_panel_ignores_non_terminal_payload() {
        let payload = json!({
            "sessions": [],
            "success": true
        });
        assert!(!should_render_command_session_terminal_panel(&payload));
    }

    #[test]
    fn command_session_terminal_panel_skips_git_diff_payload() {
        let payload = json!({
            "command": "git diff -- src/main.rs",
            "output": "diff --git a/src/main.rs b/src/main.rs",
            "content_type": "git_diff"
        });
        assert!(!should_render_command_session_terminal_panel(&payload));
    }

    #[test]
    fn preferred_follow_up_rendered_body_prefers_output_over_content() {
        let payload = json!({
            "output": "stdout body",
            "content": "content body"
        });

        assert_eq!(preferred_follow_up_rendered_body(&payload), Some("stdout body"));
    }

    #[test]
    fn preferred_follow_up_rendered_body_falls_back_to_content() {
        let payload = json!({
            "content": "content body"
        });

        assert_eq!(preferred_follow_up_rendered_body(&payload), Some("content body"));
    }

    #[tokio::test]
    async fn render_tool_output_command_session_git_diff_renders_diff_not_command_preview() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "git diff -- src/main.rs",
            "output": "diff --git a/src/main.rs b/src/main.rs\n+added\n-removed\n",
            "content_type": "git_diff",
            "is_exited": true,
            "exit_code": 0
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::UNIFIED_EXEC), &payload, None)
            .await
            .expect("git diff payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("diff --git a/src/main.rs b/src/main.rs"));
        assert!(!inline_output.contains("└ "), "run-command preview prefix should not appear for git diff payload");
    }

    #[tokio::test]
    async fn render_tool_output_command_session_git_diff_stdout_renders_diff_not_command_preview() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "git diff -- src/lib.rs",
            "stdout": "diff --git a/src/lib.rs b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            "content_type": "git_diff",
            "is_exited": true,
            "exit_code": 0
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::UNIFIED_EXEC), &payload, None)
            .await
            .expect("git diff stdout payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(inline_output.contains("@@ -1 +1 @@"));
        assert!(inline_output.contains("new"));
        assert!(!inline_output.contains("└ "), "run-command preview prefix should not appear for git diff payload");
    }

    #[tokio::test]
    async fn render_tool_output_apply_patch_renders_diff_content() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "success": true,
            "diff": [{
                "path": "README.md",
                "content": "diff --git a/README.md b/README.md\n-before\n+after\n",
                "skipped": false
            }]
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::APPLY_PATCH), &payload, None)
            .await
            .expect("apply_patch diff payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("README.md"));
        assert!(inline_output.contains("-before"));
        assert!(inline_output.contains("+after"));
    }

    #[tokio::test]
    async fn render_tool_output_apply_patch_parses_ansi_diff_payloads() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "success": true,
            "diff": [{
                "path": "README.md",
                "operation": "updated",
                "content": "\u{1b}[36mdiff --git a/README.md b/README.md\u{1b}[0m\n\u{1b}[36m@@ -1 +1 @@\u{1b}[0m\n\u{1b}[31m-before\u{1b}[0m\n\u{1b}[32m+after\u{1b}[0m\n",
                "additions": 1,
                "deletions": 1,
                "skipped": false
            }]
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::APPLY_PATCH), &payload, None)
            .await
            .expect("ANSI apply_patch diff payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("-    1 │ before"));
        assert!(inline_output.contains("+    1 │ after"));
    }

    #[tokio::test]
    async fn render_tool_output_command_session_renders_structured_hints() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "cargo check",
            "output": "tail preview",
            "session_id": "run-123",
            "is_exited": false,
            "next_continue_args": {
                "session_id": "run-123"
            },
            "spool_path": ".vtcode/context/tool_outputs/run-123.txt"
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::UNIFIED_EXEC), &payload, None)
            .await
            .expect("structured hint payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Large output was spooled to"));
        assert!(inline_output.contains("exec_command"));
        assert!(inline_output.contains("cat, sed, or rg"));
        assert!(!inline_output.contains("read_file/grep_file"));
        assert!(inline_output.contains("Reuse `next_continue_args`."));
    }

    #[tokio::test]
    async fn render_tool_output_exec_command_renders_terminal_panel_with_output() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        renderer.set_tool_display_mode(ToolDisplayMode::Expanded);
        let payload = json!({
            "command": "cargo check",
            "output": "Compiling vtcode v0.135.9",
            "stdout": "Compiling vtcode v0.135.9",
            "stderr": "",
            "is_exited": true,
            "exit_code": 0
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::EXEC_COMMAND), &payload, None)
            .await
            .expect("exec_command payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(
            inline_output.contains("Compiling vtcode v0.135.9"),
            "exec_command output must be rendered in the terminal panel, got: {inline_output}"
        );
        assert!(
            !inline_output.contains("(no output)"),
            "exec_command output must not fall through to the no-output status renderer"
        );
    }

    #[tokio::test]
    async fn render_tool_output_exec_command_compact_hides_completed_stdout() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "cargo check",
            "stdout": "verbose completed output",
            "stderr": ""
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::EXEC_COMMAND), &payload, None)
            .await
            .expect("exec_command payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(!inline_output.contains("verbose completed output"));
    }

    #[tokio::test]
    async fn render_tool_output_exec_pty_cmd_renders_terminal_panel_with_output() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "ls -la",
            "output": "total 0",
            "session_id": "run-456",
            "is_exited": true,
            "exit_code": 0
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::EXEC_PTY_CMD), &payload, None)
            .await
            .expect("exec_pty_cmd payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(
            inline_output.contains("total") && inline_output.contains("✓ exit 0"),
            "exec_pty_cmd output must be rendered in the terminal panel, got: {inline_output}"
        );
    }

    #[tokio::test]
    async fn render_tool_output_run_pty_completed_spooled_output_is_reference_only() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "cargo check",
            "output": "preview text that should not render inline",
            "session_id": "run-123",
            "is_exited": true,
            "exit_code": 0,
            "spool_path": ".vtcode/context/tool_outputs/run-123.txt"
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::RUN_PTY_CMD), &payload, None)
            .await
            .expect("spooled PTY payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("✓ exit 0"));
        assert!(inline_output.contains("Large output was spooled to"));
        assert!(!inline_output.contains("preview text that should not render inline"));
        assert!(!inline_output.contains("(no output)"));
    }

    #[tokio::test]
    async fn render_tool_output_read_file_renders_spool_hint_on_early_return_path() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "path": "README.md",
            "content": "preview",
            "spool_path": ".vtcode/context/tool_outputs/readme.txt"
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::READ_FILE), &payload, None)
            .await
            .expect("read_file payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Large output was spooled to"));
        assert!(inline_output.contains("exec_command"));
        assert!(inline_output.contains("cat, sed, or rg"));
        assert!(!inline_output.contains("read_file/grep_file"));
    }

    #[tokio::test]
    async fn render_tool_output_web_fetch_content_fallback_renders_follow_up_hint() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "content": "preview",
            "spool_path": ".vtcode/context/tool_outputs/web.txt"
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::WEB_FETCH), &payload, None)
            .await
            .expect("web_fetch payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Large output was spooled to"));
        assert!(inline_output.contains("exec_command"));
        assert!(inline_output.contains("cat, sed, or rg"));
        assert!(!inline_output.contains("read_file/grep_file"));
    }

    #[tokio::test]
    async fn render_tool_output_does_not_duplicate_spooled_output_hint() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let spool_path = ".vtcode/context/tool_outputs/web.txt";
        let hint = spooled_output_hint(spool_path);
        let payload = json!({
            "output": hint,
            "spool_path": spool_path
        });

        render_tool_output(&mut renderer, Some("custom_tool"), &payload, None)
            .await
            .expect("spooled hint payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert_eq!(inline_output.matches("Large output was spooled to").count(), 1);
        assert!(inline_output.contains("exec_command"));
        assert!(inline_output.contains("cat, sed, or rg"));
    }

    #[tokio::test]
    async fn render_tool_output_read_file_long_preview_keeps_preview_limits() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let content = (1..=100).map(|idx| format!("{idx}: line {idx}")).collect::<Vec<_>>().join("\n");
        let payload = json!({
            "path": "src/main.rs",
            "content": content
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::READ_FILE), &payload, None)
            .await
            .expect("read_file preview payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        // read_file now shows a summary line instead of code preview
        assert!(inline_output.contains("Read 100 lines"));
        assert!(inline_output.contains("└ Read 100 lines"));
        assert!(!inline_output.contains("    Read 100 lines"));
    }

    #[tokio::test]
    async fn render_tool_output_renders_loop_recovery_hint_from_structured_fields() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "loop_detected": true,
            "fallback_tool": vtcode_core::config::constants::tools::CODE_SEARCH
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::CODE_SEARCH), &payload, None)
            .await
            .expect("loop recovery hint payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Loop detected; fallback is available."));
    }

    #[tokio::test]
    async fn render_tool_output_renders_spooled_loop_recovery_hint() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "loop_detected": true,
            "spool_path": ".vtcode/context/tool_outputs/readme.txt",
            "next_read_args": {
                "path": ".vtcode/context/tool_outputs/readme.txt",
                "offset": 81,
                "limit": 40
            }
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::READ_FILE), &payload, None)
            .await
            .expect("spooled loop recovery hint payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Loop detected; continue from spooled output."));
    }

    #[tokio::test]
    async fn render_tool_output_does_not_duplicate_loop_recovery_hint() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "loop_detected": true,
            "fallback_tool": vtcode_core::config::constants::tools::CODE_SEARCH,
            "output": "Loop detected; fallback is available."
        });

        render_tool_output(&mut renderer, Some("custom_tool"), &payload, None)
            .await
            .expect("duplicate hint payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert_eq!(inline_output.matches("Loop detected; fallback is available.").count(), 1);
    }

    #[tokio::test]
    async fn render_tool_output_command_session_keeps_exit_127_output_and_guidance() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "command": "pip install pymupdf",
            "output": "bash: pip: command not found",
            "session_id": "run-127",
            "is_exited": true,
            "exit_code": 127,
            "critical_note": "Command `pip` was not found in PATH.",
            "next_action": "Check the command name or install the missing binary, then rerun the command."
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::UNIFIED_EXEC), &payload, None)
            .await
            .expect("exit 127 payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("bash: pip: command not found"));
        assert!(inline_output.contains("not found in PATH."));
        assert!(
            inline_output.contains("Check the command name or install the missing binary, then rerun the command.")
        );
        assert!(inline_output.contains("✓ exit 127"));
        assert!(!inline_output.contains("Solution:"));
        assert_eq!(
            inline_output
                .matches("Check the command name or install the missing binary, then rerun the command.")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn render_tool_output_renders_generic_recoverable_failure_guidance() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "error": "Tool preflight validation failed: x",
            "is_recoverable": true,
            "next_action": "Retry with fallback_tool_args."
        });

        render_tool_output(&mut renderer, Some("custom_tool"), &payload, None)
            .await
            .expect("generic recoverable failure should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("Tool preflight validation failed: x"));
        assert!(inline_output.contains("Retry with fallback_tool_args."));
        assert_eq!(inline_output.matches("Retry with fallback_tool_args.").count(), 1);
        assert!(!inline_output.contains("\"error\""));
        assert!(!inline_output.contains("\"next_action\""));
    }

    #[tokio::test]
    async fn render_tool_output_write_file_diff_truncation_uses_file_operation_hint() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "diff_preview": {
                "content": "@@ -1 +1 @@\n-old\n+new\n",
                "truncated": true,
                "omitted_line_count": 5
            }
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::WRITE_FILE), &payload, None)
            .await
            .expect("write file diff payload should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("use exec_command with sed for full view"));
        assert!(!inline_output.contains("use read_file for full view"));
    }

    #[tokio::test]
    async fn render_tool_output_write_file_uses_canonical_diff_entries() {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut renderer = AnsiRenderer::with_inline_ui(InlineHandle::new_for_tests(sender), Default::default());
        let payload = json!({
            "path": "README.md",
            "diff": [{
                "path": "README.md",
                "operation": "updated",
                "content": "@@ -1 +1 @@\n-before\n+after\n",
                "additions": 1,
                "deletions": 1,
                "truncated": false,
                "skipped": false
            }]
        });

        render_tool_output(&mut renderer, Some(vtcode_core::config::constants::tools::WRITE_FILE), &payload, None)
            .await
            .expect("canonical write diff should render");

        let inline_output = collect_inline_output(&mut receiver);
        assert!(inline_output.contains("• Edited README.md (+1 -1)"));
        assert!(inline_output.contains("-    1 │ before"));
        assert!(inline_output.contains("+    1 │ after"));
    }

    #[test]
    fn tracker_summary_lines_hide_successful_tracker_details() {
        let payload = json!({
            "status": "updated",
            "message": "Item 2 status changed: pending -> in_progress",
            "checklist": {
                "total": 4,
                "completed": 1,
                "in_progress": 2,
                "pending": 1,
                "blocked": 0,
                "progress_percent": 25,
                "items": [
                    { "index": 1, "description": "A", "status": "completed" },
                    { "index": 2, "description": "B", "status": "in_progress" },
                    { "index": 3, "description": "C", "status": "in_progress" },
                    { "index": 4, "description": "D", "status": "pending" }
                ]
            }
        });

        assert!(tracker_summary_lines(&payload).is_empty());
    }

    #[test]
    fn tracker_summary_lines_still_show_message_without_checklist() {
        let payload = json!({
            "status": "empty",
            "message": "No active checklist."
        });
        let lines = tracker_summary_lines(&payload);
        assert!(lines.iter().any(|line| line == "  Tracker status: empty"));
        assert!(lines.iter().any(|line| line == "  Update: No active checklist."));
    }

    #[test]
    fn tracker_view_lines_formats_flat_leaf_statuses_as_a_compact_tree() {
        // A status-to-glyph regression here would make the active plan harder
        // to scan; rows must stay compact and show all leaf states directly.
        let payload = json!({
            "status": "updated",
            "checklist": {
                "title": "Release",
                "items": [
                    { "index_path": "1", "description": "Investigate", "status": "pending" },
                    { "index_path": "2", "description": "Implement", "status": "in_progress" },
                    { "index_path": "3", "description": "Verify", "status": "completed" },
                    { "index_path": "4", "description": "Resolve dependency", "status": "blocked" }
                ]
            }
        });

        let rows = tracker_view_lines(&payload);

        assert_eq!(
            rows,
            vec![
                "• Release",
                "  ├ □ Investigate",
                "  ├ [-] Implement",
                "  ├ [x] Verify",
                "  └ [!] Resolve dependency",
            ]
        );
    }

    #[test]
    fn tracker_view_lines_formats_hierarchy_without_status_for_parent_rows() {
        // Parents summarize their children, so showing their stored leaf status
        // would be misleading. Metadata remains in the structured payload but
        // must not turn into visible detail rows.
        let payload = json!({
            "status": "updated",
            "checklist": {
                "title": "Release",
                "items": [
                    {
                        "index_path": "1",
                        "level": 0,
                        "description": "Prepare release",
                        "status": "in_progress",
                        "files": ["Cargo.toml"],
                        "outcome": "Version is ready",
                        "verify": ["cargo nextest run -p vtcode"]
                    },
                    { "index_path": "1.1", "level": 1, "description": "Update version", "status": "completed" },
                    { "index_path": "1.2", "level": 1, "description": "Run checks", "status": "in_progress" },
                    { "index_path": "2", "level": 0, "description": "Publish", "status": "pending" }
                ]
            }
        });

        let rows = tracker_view_lines(&payload);

        assert_eq!(
            rows,
            vec![
                "• Release",
                "  ├ Prepare release",
                "  │ [x] Update version",
                "  │ [-] Run checks",
                "  └ □ Publish",
            ]
        );
        assert!(
            rows.iter()
                .all(|line| { !line.contains("files:") && !line.contains("outcome:") && !line.contains("verify:") })
        );
        assert_eq!(payload["checklist"]["items"][0]["files"], json!(["Cargo.toml"]));
        assert_eq!(payload["checklist"]["items"][0]["outcome"], "Version is ready");
        assert_eq!(payload["checklist"]["items"][0]["verify"], json!(["cargo nextest run -p vtcode"]));
        let metadata = tracker_panel_metadata(&json!({
            "checklist": {
                "title": "Release",
                "completed": 1,
                "total": 4
            }
        }))
        .expect("structured panel metadata");
        assert_eq!(metadata.title, "Release");
        assert_eq!((metadata.completed, metadata.total), (1, 4));
    }

    #[test]
    fn tracker_view_lines_keeps_diagnostics_for_empty_or_malformed_tracker_responses() {
        // Compact rendering applies only to successful structured checklists.
        // Empty and malformed responses must remain diagnosable instead of
        // silently presenting a blank task panel.
        let empty = json!({});
        let malformed = json!({
            "status": "error",
            "message": "Tracker response did not include checklist items.",
            "view": { "lines": "not an array" }
        });
        let malformed_items = json!({
            "status": "error",
            "message": "Tracker response contained invalid checklist items.",
            "checklist": { "items": [{}] }
        });

        assert!(tracker_view_lines(&empty).is_empty());
        assert_eq!(
            tracker_view_lines(&malformed),
            vec![
                "• Task tracker",
                "  Tracker status: error",
                "  Update: Tracker response did not include checklist items.",
            ]
        );
        assert_eq!(
            tracker_view_lines(&malformed_items),
            vec![
                "• Task tracker",
                "  Tracker status: error",
                "  Update: Tracker response contained invalid checklist items.",
            ]
        );

        let partial_failure = json!({
            "status": "error",
            "message": "Tracker response was only partially applied.",
            "checklist": {
                "items": [
                    { "index": 1, "description": "Still present", "status": "completed" }
                ]
            }
        });
        assert_eq!(
            tracker_view_lines(&partial_failure),
            vec![
                "• Task tracker",
                "  Tracker status: error",
                "  Update: Tracker response was only partially applied.",
                "  └ [x] Still present",
            ]
        );
    }
}
