use crate::agent::runloop::git::normalize_workspace_path;
use crate::agent::runloop::mcp_events::McpPanelState;
use crate::agent::runloop::unified::state::SessionStats;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use vtcode_commons::paths::ensure_path_within_workspace_resolved;
use vtcode_core::config::ToolDisplayMode;
use vtcode_core::config::constants::tools;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::tools::command_args;
use vtcode_core::tools::tool_intent;
use vtcode_core::utils::ansi::AnsiRenderer;
use vtcode_core::utils::ansi::MessageStyle;
use vtcode_core::utils::style_helpers::ColourPalette;
use vtcode_core::utils::transcript;
use vtcode_ui::tui::app::{InlineHandle, InlineMessageKind, InlineSegment, InlineTextStyle, ToolOutputId};

use crate::agent::runloop::unified::run_loop_context::RunLoopContext;
use crate::agent::runloop::unified::tool_pipeline::{
    ToolDisplayStatus, ToolExecutionStatus, ToolPipelineOutcome, renders_pty_command_header, streams_pty_output,
};
use crate::agent::runloop::unified::tool_summary_helpers::{
    display_command_text, preview_command, relativize_command_paths,
};
use vtcode_commons::canonicalize;

fn record_mcp_outcome_event(
    mcp_panel_state: &mut McpPanelState,
    tool_name: &str,
    args_val: &serde_json::Value,
    command_success: bool,
) {
    let mut mcp_event = crate::agent::runloop::mcp_events::McpEvent::new(
        "mcp".to_string(),
        tool_name.to_string(),
        Some(args_val.to_string()),
    );
    if command_success {
        mcp_event.success(None);
    } else {
        mcp_event.failure(Some("Command returned a non-zero exit code".to_string()));
    }
    mcp_panel_state.add_event(mcp_event);
}

fn collect_modified_files(modified_files: &[String]) -> Vec<PathBuf> {
    modified_files.iter().map(PathBuf::from).collect()
}

fn collect_instruction_activity_paths(
    workspace_root: &Path,
    args_val: &serde_json::Value,
    output: &serde_json::Value,
    modified_files: &[String],
) -> Vec<PathBuf> {
    let canonical_workspace = canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    let mut paths = BTreeSet::new();
    for modified in modified_files {
        push_activity_path(workspace_root, &canonical_workspace, modified, &mut paths);
    }
    collect_paths_from_value(workspace_root, &canonical_workspace, Some("args"), args_val, &mut paths);
    collect_paths_from_value(workspace_root, &canonical_workspace, Some("output"), output, &mut paths);
    paths.into_iter().collect()
}

fn collect_paths_from_value(
    workspace_root: &Path,
    canonical_workspace: &Path,
    key: Option<&str>,
    value: &serde_json::Value,
    paths: &mut BTreeSet<PathBuf>,
) {
    match value {
        serde_json::Value::String(text) => {
            if key.is_some_and(path_like_key) {
                push_activity_path(workspace_root, canonical_workspace, text, paths);
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                collect_paths_from_value(workspace_root, canonical_workspace, key, value, paths);
            }
        }
        serde_json::Value::Object(map) => {
            for (child_key, child_value) in map {
                collect_paths_from_value(
                    workspace_root,
                    canonical_workspace,
                    Some(child_key.as_str()),
                    child_value,
                    paths,
                );
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn path_like_key(key: &str) -> bool {
    matches!(
        key,
        "path"
            | "paths"
            | "file"
            | "files"
            | "file_path"
            | "file_paths"
            | "cwd"
            | "workdir"
            | "directory"
            | "directories"
            | "root"
            | "workspace"
    )
}

fn push_activity_path(workspace_root: &Path, canonical_workspace: &Path, raw: &str, paths: &mut BTreeSet<PathBuf>) {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.contains("://") || trimmed.starts_with("untitled:") {
        return;
    }

    let normalized = normalize_workspace_path(workspace_root, Path::new(trimmed));
    if normalized.starts_with(canonical_workspace) || normalized.starts_with(workspace_root) {
        paths.insert(normalized);
    }
}

fn is_run_pty_tool(name: &str, args_val: &serde_json::Value) -> bool {
    renders_pty_command_header(name, args_val)
}

fn is_command_output_call(name: &str, args_val: &serde_json::Value) -> bool {
    name != tools::SEND_PTY_INPUT
        && (name == tools::EXECUTE_CODE
            || tool_intent::is_command_run_tool_call(name, args_val)
            || is_run_pty_tool(name, args_val))
}

fn compact_run_completion_line(output: &serde_json::Value, status: ToolDisplayStatus) -> Option<String> {
    if let Some(exit_code) = output.get("exit_code").and_then(serde_json::Value::as_i64) {
        if matches!(status, ToolDisplayStatus::Success) && exit_code == 0 {
            return Some("✓ run completed (exit code: 0)".to_string());
        }
        if matches!(status, ToolDisplayStatus::Warning) && exit_code == 0 {
            return Some("⚠ run completed with warnings (exit code: 0)".to_string());
        }
        return Some(format!("✗ run error, exit code: {exit_code}"));
    }

    if output.get("is_exited").and_then(serde_json::Value::as_bool) == Some(true) {
        if matches!(status, ToolDisplayStatus::Success) {
            return Some("✓ done".to_string());
        }
        if matches!(status, ToolDisplayStatus::Warning) {
            return Some("⚠ done with warnings".to_string());
        }
        return Some("✗ failed".to_string());
    }

    match status {
        ToolDisplayStatus::Failure => Some("✗ failed".to_string()),
        ToolDisplayStatus::Warning => Some("⚠ completed with warnings".to_string()),
        ToolDisplayStatus::Success => None,
    }
}

fn is_git_diff_payload(output: &serde_json::Value) -> bool {
    output
        .get("content_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|content_type| content_type == "git_diff")
}

fn has_renderable_stream_content(output: &serde_json::Value) -> bool {
    ["output", "stdout", "stderr", "content"].iter().any(|key| {
        output
            .get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    })
}

fn is_task_tracker_tool(name: &str) -> bool {
    matches!(name, tools::TASK_TRACKER)
}

fn task_tracker_block_lines(output: &serde_json::Value) -> Vec<String> {
    crate::agent::runloop::tool_output::tracker_view_lines(output)
}

fn task_tracker_block_segments(lines: &[String]) -> Vec<Vec<InlineSegment>> {
    let style = std::sync::Arc::new(InlineTextStyle::default());
    lines
        .iter()
        .map(|line| vec![InlineSegment { text: line.clone(), style: style.clone() }])
        .collect()
}

fn apply_task_tracker_block(
    handle: &InlineHandle,
    harness_state: &mut crate::agent::runloop::unified::run_loop_context::HarnessTurnState,
    lines: Vec<String>,
) {
    let replace_count = harness_state.replaceable_task_tracker_count();
    let segments = task_tracker_block_segments(&lines);

    if let Some(count) = replace_count {
        handle.replace_last(count, InlineMessageKind::Tool, segments);
        transcript::replace_last(count, &lines);
    } else {
        for (segments, plain_line) in segments.into_iter().zip(lines.iter()) {
            handle.append_line(InlineMessageKind::Tool, segments);
            transcript::append(plain_line);
        }
    }

    harness_state.remember_task_tracker_block(lines);
}

/// Extract the command string from tool call arguments.
fn extract_command_line(args: &serde_json::Value) -> Option<String> {
    command_args::command_text(args).ok().flatten()
}

fn compact_command_text(name: &str, args: &serde_json::Value, workspace_root: Option<&Path>) -> String {
    // Display join (no shell_words quoting) plus a first-line head-truncated
    // preview: the collapsed row must stay readable, not executable-looking.
    display_command_text(args)
        .map(|command| relativize_command_paths(&command, workspace_root))
        .map(|command| preview_command(&command, 120))
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| name.to_string())
}

fn compact_hidden_line_count(output: &serde_json::Value, complete_capture: Option<&str>) -> usize {
    if let Some(capture) = complete_capture {
        return normalize_terminal_output_lines(capture).len();
    }

    canonical_pipe_streams(output)
        .into_iter()
        .map(|stream| {
            if stream.label == Some("stderr") {
                return 0;
            }

            let line_count = normalize_terminal_output_lines(stream.text).len();
            if stream.label.is_none()
                && let Some(stderr) = output_text(output, "stderr")
                && streams_are_aliases(stream.text, stderr)
            {
                return line_count.saturating_sub(normalize_terminal_output_lines(stderr).len());
            }
            line_count
        })
        .sum()
}

fn render_command_summary(
    renderer: &mut AnsiRenderer,
    name: &str,
    args_val: &serde_json::Value,
    output: &serde_json::Value,
    command_success: bool,
    workspace_root: Option<&Path>,
    viewer_id: Option<ToolOutputId>,
    force_expanded: bool,
) -> Result<()> {
    if let Some(viewer_id) = viewer_id {
        // Carry the identity on the summary command itself. Text/order
        // matching is ambiguous when async calls run the same command.
        renderer.set_next_tool_output_anchor(viewer_id);
    }
    let stream_label = crate::agent::runloop::unified::tool_summary::stream_label_from_output(output, command_success);
    let summary_ctx = crate::agent::runloop::unified::tool_summary::ToolSummaryRenderContext { workspace_root };
    let status = ToolDisplayStatus::from_command_output(output, command_success);
    let bullet_colour = status.colour(ColourPalette::default());
    if force_expanded {
        crate::agent::runloop::unified::tool_summary::render_expanded_tool_call_summary(
            renderer,
            name,
            args_val,
            stream_label,
            &summary_ctx,
            bullet_colour,
        )
    } else {
        crate::agent::runloop::unified::tool_summary::render_tool_call_summary(
            renderer,
            name,
            args_val,
            stream_label,
            &summary_ctx,
            bullet_colour,
        )
    }
}

fn value_has_content(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(value) => *value,
        serde_json::Value::String(value) => !value.trim().is_empty(),
        serde_json::Value::Array(values) => !values.is_empty(),
        serde_json::Value::Object(values) => !values.is_empty(),
        serde_json::Value::Number(_) => true,
    }
}

const STRUCTURED_COMMAND_OUTPUT_FIELDS: &[&str] = &[
    "output",
    "stdout",
    "stderr",
    "content",
    "command",
    "critical_note",
    "next_action",
    "exit_code",
];

const COMPACT_COMMAND_ARTEFACT_FIELDS: &[&str] = &[
    "generated_files",
    "json_result",
    "modified_files",
    "diff",
    "diff_preview",
    "failure_diagnostics",
    "security_notice",
    "artifacts",
];

fn structured_command_context(output: &serde_json::Value) -> Option<String> {
    let object = output.as_object()?;
    let metadata = object
        .iter()
        .filter(|(key, value)| {
            !STRUCTURED_COMMAND_OUTPUT_FIELDS.contains(&key.as_str()) && !matches!(value, serde_json::Value::Null)
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<serde_json::Map<_, _>>();

    if metadata.is_empty() {
        return None;
    }

    serde_json::to_string_pretty(&serde_json::Value::Object(metadata)).ok()
}

fn append_structured_command_context(lines: &mut Vec<String>, output: &serde_json::Value) {
    let Some(context) = structured_command_context(output) else {
        return;
    };

    lines.push("  structured output:".to_string());
    lines.extend(context.lines().map(|line| format!("    {line}")));
}

fn render_structured_command_context(renderer: &mut AnsiRenderer, output: &serde_json::Value) -> Result<()> {
    let Some(context) = structured_command_context(output) else {
        return Ok(());
    };

    renderer.line(MessageStyle::ToolDetail, "structured output:")?;
    for line in context.lines() {
        renderer.line(MessageStyle::ToolDetail, &format!("  {line}"))?;
    }
    Ok(())
}

fn complete_capture_unavailable(output: &serde_json::Value, complete_capture: Option<&str>) -> bool {
    output.get("spool_path").is_some() && complete_capture.is_none()
}

fn has_compact_command_artefact(output: &serde_json::Value, complete_capture: Option<&str>) -> bool {
    output_text(output, "critical_note").is_some()
        || stderr_for_inline_display(output).is_some()
        || complete_capture_unavailable(output, complete_capture)
        || COMPACT_COMMAND_ARTEFACT_FIELDS
            .iter()
            .any(|key| output.get(*key).is_some_and(value_has_content))
        || [
            "security_notice",
            "next_action",
            "next_continue_args",
            "next_read_args",
            "fallback_tool",
            "fallback_tool_args",
        ]
        .iter()
        .any(|key| output.get(*key).is_some_and(value_has_content))
        || output.get("loop_detected").and_then(serde_json::Value::as_bool) == Some(true)
}

fn has_file_operation_diff(output: &serde_json::Value) -> bool {
    !vtcode_core::tools::file_ops::canonical_diff_previews(output).is_empty()
}

fn warning_message(output: &serde_json::Value) -> Option<String> {
    let warning = output.get("warning")?;
    match warning {
        serde_json::Value::String(message) => {
            let message = message.trim();
            (!message.is_empty()).then(|| message.to_string())
        }
        serde_json::Value::Number(number) if number.as_f64().is_some_and(|value| value != 0.0) => {
            Some(format!("warning count: {number}"))
        }
        serde_json::Value::Bool(true) => Some("completed with warnings".to_string()),
        serde_json::Value::Array(values) if !values.is_empty() => Some("completed with warnings".to_string()),
        serde_json::Value::Object(values) if !values.is_empty() => {
            let message = warning
                .as_object()
                .and_then(|fields| fields.get("message"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty());
            Some(message.unwrap_or("completed with warnings").to_string())
        }
        _ => None,
    }
}

fn append_warning_line(lines: &mut Vec<String>, output: &serde_json::Value) {
    if let Some(message) = warning_message(output) {
        lines.push(format!("    ⚠ {message}"));
    }
}

fn append_capture_status_line(lines: &mut Vec<String>, output: &serde_json::Value, complete_capture: Option<&str>) {
    if complete_capture_unavailable(output, complete_capture) {
        lines.push("    Complete command output capture unavailable.".to_string());
    }
}

/// Record the tool-call summary line ("• Ran ...") to the transcript only.
fn record_summary_line(name: &str, args: &serde_json::Value, _output: &serde_json::Value, _command_success: bool) {
    let action_label = if tool_intent::is_command_run_tool_call(name, args) {
        "Run command"
    } else {
        name
    };
    let headline = if action_label == "Run command" {
        if let Some(cmd) = extract_command_line(args) {
            format!("Ran {cmd}")
        } else {
            "Ran command".to_string()
        }
    } else {
        format!("• {action_label}")
    };
    transcript::append(&headline);
}

fn contains_line_block(container: &str, candidate: &str) -> bool {
    !line_block_ranges(container, candidate).is_empty()
}

fn line_block_ranges(container: &str, candidate: &str) -> Vec<(usize, usize)> {
    let container_lines = container.lines().collect::<Vec<_>>();
    let candidate_lines = candidate.lines().collect::<Vec<_>>();
    if candidate_lines.is_empty() || candidate_lines.len() > container_lines.len() {
        return Vec::new();
    }

    container_lines
        .windows(candidate_lines.len())
        .enumerate()
        .filter_map(|(start, window)| {
            (window == candidate_lines.as_slice()).then_some((start, start + candidate_lines.len()))
        })
        .collect()
}

fn contains_distinct_line_blocks(container: &str, first: &str, second: &str) -> bool {
    let first_ranges = line_block_ranges(container, first);
    let second_ranges = line_block_ranges(container, second);
    first_ranges.iter().any(|&(first_start, first_end)| {
        second_ranges
            .iter()
            .any(|&(second_start, second_end)| first_end <= second_start || second_end <= first_start)
    })
}

fn streams_are_aliases(left: &str, right: &str) -> bool {
    contains_line_block(left, right) || contains_line_block(right, left)
}

fn output_text<'a>(output: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    output
        .get(key)
        .and_then(serde_json::Value::as_str)
        .map(str::trim_end)
        .filter(|text| !text.trim().is_empty())
}

fn stderr_for_inline_display(output: &serde_json::Value) -> Option<&str> {
    let stderr = output_text(output, "stderr")?;
    // Named streams are distinct unless the authoritative merged `output`
    // field proves that stderr is already present in the terminal capture.
    // stdout and stderr can legitimately contain identical text.
    let already_visible = output_text(output, "output").is_some_and(|merged| {
        output_text(output, "stdout").map_or_else(
            || contains_line_block(merged, stderr),
            |stdout| contains_distinct_line_blocks(merged, stdout, stderr),
        )
    });
    if already_visible { None } else { Some(stderr) }
}

fn ordered_stream_texts(output: &serde_json::Value) -> Vec<&str> {
    canonical_pipe_streams(output).into_iter().map(|stream| stream.text).collect()
}

#[derive(Clone, Copy)]
struct CanonicalOutputStream<'a> {
    label: Option<&'static str>,
    text: &'a str,
}

fn append_named_streams<'a>(
    streams: &mut Vec<CanonicalOutputStream<'a>>,
    stdout: Option<&'a str>,
    stderr: Option<&'a str>,
) {
    if let Some(stdout) = stdout {
        streams.push(CanonicalOutputStream { label: Some("stdout"), text: stdout });
    }
    if let Some(stderr) = stderr {
        streams.push(CanonicalOutputStream { label: Some("stderr"), text: stderr });
    }
}

fn append_content_stream<'a>(streams: &mut Vec<CanonicalOutputStream<'a>>, content: Option<&'a str>) {
    let Some(content) = content else {
        return;
    };
    if !streams.iter().any(|stream| contains_line_block(stream.text, content)) {
        streams.push(CanonicalOutputStream { label: None, text: content });
    }
}

fn canonical_pipe_streams(output: &serde_json::Value) -> Vec<CanonicalOutputStream<'_>> {
    let merged = output_text(output, "output");
    let stdout = output_text(output, "stdout");
    let stderr = output_text(output, "stderr");
    let content = output_text(output, "content");
    let mut streams = Vec::new();

    if let Some(merged) = merged {
        let stdout_is_in_merged = stdout.is_some_and(|text| contains_line_block(merged, text));
        let stderr_is_in_merged = stderr.is_some_and(|text| contains_line_block(merged, text));
        let stdout_contains_merged = stdout.is_some_and(|text| contains_line_block(text, merged));
        let stderr_contains_merged = stderr.is_some_and(|text| contains_line_block(text, merged));

        // A combined `output` field is authoritative when it contains both
        // named streams as distinct blocks. Requiring non-overlapping blocks
        // matters when stdout and stderr happen to be identical or one is a
        // prefix of the other: one occurrence cannot prove both are copies.
        if let (Some(stdout), Some(stderr)) = (stdout, stderr)
            && contains_distinct_line_blocks(merged, stdout, stderr)
        {
            streams.push(CanonicalOutputStream { label: None, text: merged });
            append_content_stream(&mut streams, content);
            return streams;
        }

        // When both named streams contain the merged value, the merged field
        // is a bounded preview. Keep each complete, labelled stream instead of
        // guessing that the single preview occurrence represents both pipes.
        if stdout_contains_merged && stderr_contains_merged {
            append_named_streams(&mut streams, stdout, stderr);
            append_content_stream(&mut streams, content);
            return streams;
        }

        // A preview nested in either one named stream is best represented by
        // the complete named values. The other stream remains labelled even
        // when its content is not present in the preview.
        if stdout_contains_merged || stderr_contains_merged {
            append_named_streams(&mut streams, stdout, stderr);
            append_content_stream(&mut streams, content);
            return streams;
        }

        // If both named streams are present in the merged field but overlap,
        // preserve their labels and retain merged-only lines when neither
        // named value covers the whole merged field.
        if stdout_is_in_merged && stderr_is_in_merged {
            append_named_streams(&mut streams, stdout, stderr);
            if !stdout_contains_merged && !stderr_contains_merged {
                streams.push(CanonicalOutputStream { label: None, text: merged });
            }
            append_content_stream(&mut streams, content);
            return streams;
        }

        // A merged value containing only one named stream still carries
        // unlabelled content. Keep that merged value and append the other
        // named stream rather than dropping it as an apparent alias.
        streams.push(CanonicalOutputStream { label: None, text: merged });
        if let Some(stdout) = stdout
            && !stdout_is_in_merged
        {
            streams.push(CanonicalOutputStream { label: Some("stdout"), text: stdout });
        }
        if let Some(stderr) = stderr
            && !stderr_is_in_merged
        {
            streams.push(CanonicalOutputStream { label: Some("stderr"), text: stderr });
        }
        append_content_stream(&mut streams, content);
        return streams;
    }

    if let Some(stdout) = stdout {
        streams.push(CanonicalOutputStream { label: Some("stdout"), text: stdout });
    }
    if let Some(stderr) = stderr {
        // Without a merged authoritative field, stdout and stderr are
        // separate pipes even when their contents happen to match.
        streams.push(CanonicalOutputStream { label: Some("stderr"), text: stderr });
    }
    append_content_stream(&mut streams, content);
    streams
}

async fn load_complete_output(output: &serde_json::Value, workspace_root: Option<&Path>) -> Option<String> {
    if output.get("spool_path").is_some() {
        let spool_path = output
            .get("spool_path")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|path| !path.is_empty())?;
        let root = workspace_root?;
        let candidate = if Path::new(spool_path).is_absolute() {
            PathBuf::from(spool_path)
        } else {
            root.join(spool_path)
        };
        let resolved = match ensure_path_within_workspace_resolved(&candidate, root).await {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(path = %candidate.display(), %error, "Rejected tool output spool path");
                return None;
            }
        };
        return match tokio::fs::read_to_string(&resolved).await {
            Ok(content) => Some(content),
            Err(error) => {
                tracing::warn!(path = %resolved.display(), %error, "Failed to read tool output spool");
                None
            }
        };
    }

    if output_text(output, "output").is_none()
        && (output_text(output, "stdout").is_some() || output_text(output, "stderr").is_some())
    {
        // Named pipe streams remain labelled in the viewer. Joining them here
        // would make the later capture renderer mistake stderr for a copy of
        // stdout and drop it.
        return None;
    }

    let texts = ordered_stream_texts(output);
    (!texts.is_empty()).then(|| texts.join("\n"))
}

fn normalize_terminal_output_lines(capture: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut chars = capture.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\x1b' => match chars.next() {
                Some('[') => {
                    let mut params = String::new();
                    let final_byte = loop {
                        let Some(next) = chars.next() else {
                            break None;
                        };
                        if ('@'..='~').contains(&next) {
                            break Some(next);
                        }
                        params.push(next);
                    };

                    match final_byte {
                        // Clear-screen sequences mean that the earlier text
                        // was only a stale terminal frame, not command output
                        // that should remain in the readable viewer.
                        Some('J') if params.starts_with('2') || params.starts_with('3') => {
                            lines.clear();
                            current.clear();
                        }
                        // Erase the current line for the common progress-bar
                        // rewrite sequence. Styling and cursor movement are
                        // intentionally omitted from the plain-text viewer.
                        Some('K') if params.starts_with('2') => current.clear(),
                        _ => {}
                    }
                }
                Some(']') => {
                    // Skip OSC title/hyperlink sequences through BEL or ST.
                    while let Some(next) = chars.next() {
                        if next == '\x07' {
                            break;
                        }
                        if next == '\x1b' && chars.peek() == Some(&'\\') {
                            let _ = chars.next();
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            },
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                    lines.push(std::mem::take(&mut current));
                } else {
                    current.clear();
                }
            }
            '\n' => lines.push(std::mem::take(&mut current)),
            '\u{8}' => {
                let _ = current.pop();
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn normalized_lines_contain_subsequence(container: &[String], candidate: &[String]) -> bool {
    if candidate.is_empty() {
        return false;
    }

    let mut candidate_index = 0;
    for line in container {
        if line == &candidate[candidate_index] {
            candidate_index += 1;
            if candidate_index == candidate.len() {
                return true;
            }
        }
    }
    false
}

fn command_output_header(name: &str, args: &serde_json::Value, workspace_root: Option<&Path>) -> String {
    let command = extract_command_line(args)
        .map(|command| vtcode_commons::formatting::collapse_whitespace(&command))
        .filter(|command| !command.is_empty())
        .map(|command| relativize_command_paths(&command, workspace_root));
    command
        .map(|command| format!("• Ran {command}"))
        .unwrap_or_else(|| format!("• Ran {name}"))
}

fn append_merged_output_lines(lines: &mut Vec<String>, output_lines: impl IntoIterator<Item = String>) {
    for (index, line) in output_lines.into_iter().enumerate() {
        if index == 0 {
            lines.push(format!("  └ {line}"));
        } else {
            lines.push(format!("    {line}"));
        }
    }
}

fn append_labelled_output_lines(lines: &mut Vec<String>, label: &str, output_lines: impl IntoIterator<Item = String>) {
    lines.push(format!("  {label}:"));
    for line in output_lines {
        lines.push(format!("    {line}"));
    }
}

fn append_viewer_status_line(lines: &mut Vec<String>, output: &serde_json::Value, status: ToolDisplayStatus) {
    if !matches!(status, ToolDisplayStatus::Success)
        && let Some(completion) = compact_run_completion_line(output, status)
    {
        lines.push(format!("    {completion}"));
    }
}

fn build_merged_command_output_lines(
    name: &str,
    args: &serde_json::Value,
    capture: &str,
    workspace_root: Option<&Path>,
    output: &serde_json::Value,
    status: ToolDisplayStatus,
) -> Vec<String> {
    let mut lines = vec![command_output_header(name, args, workspace_root)];
    let named_streams = canonical_pipe_streams(output);
    let capture_lines = normalize_terminal_output_lines(capture);
    let has_spool_metadata = output.get("spool_path").is_some();

    if has_spool_metadata {
        // A successfully loaded spool is the complete capture; the inline
        // output field is only a bounded preview and must not be shown beside
        // it. If the spool could not be loaded, keep the path fail-closed and
        // avoid presenting the untrusted preview as complete output.
        if !capture_lines.is_empty() {
            append_merged_output_lines(&mut lines, capture_lines.clone());
            for stream in &named_streams {
                let Some(label) = stream.label else {
                    continue;
                };
                let stream_lines = normalize_terminal_output_lines(stream.text);
                if !normalized_lines_contain_subsequence(&capture_lines, &stream_lines) {
                    append_labelled_output_lines(&mut lines, label, stream_lines);
                }
            }
        }
    } else if named_streams.is_empty() {
        append_merged_output_lines(&mut lines, capture_lines);
    } else {
        for stream in &named_streams {
            let output_lines = normalize_terminal_output_lines(stream.text);
            if let Some(label) = stream.label {
                append_labelled_output_lines(&mut lines, label, output_lines);
            } else {
                append_merged_output_lines(&mut lines, output_lines);
            }
        }

        // A PTY spool can contain terminal data not represented by the named
        // pipe fields. Preserve that extra capture explicitly, but do not use
        // it to deduplicate stdout and stderr when no merged field is present.
        let named_lines = named_streams
            .iter()
            .flat_map(|stream| normalize_terminal_output_lines(stream.text))
            .collect::<Vec<_>>();
        if !capture_lines.is_empty() && capture_lines != named_lines {
            append_labelled_output_lines(&mut lines, "output", capture_lines);
        }
    }
    if let Some(note) = output_text(output, "critical_note") {
        lines.push(format!("    {note}"));
    }
    append_warning_line(&mut lines, output);
    append_viewer_status_line(&mut lines, output, status);
    append_structured_command_context(&mut lines, output);
    lines
}

fn build_pipe_command_output_lines(
    name: &str,
    args: &serde_json::Value,
    output: &serde_json::Value,
    workspace_root: Option<&Path>,
    status: ToolDisplayStatus,
) -> Vec<String> {
    let mut lines = vec![command_output_header(name, args, workspace_root)];
    for stream in canonical_pipe_streams(output) {
        let output_lines = normalize_terminal_output_lines(stream.text);
        if output_lines.is_empty() {
            continue;
        }
        if let Some(label) = stream.label {
            append_labelled_output_lines(&mut lines, label, output_lines);
        } else {
            append_merged_output_lines(&mut lines, output_lines);
        }
    }
    if let Some(note) = output_text(output, "critical_note") {
        lines.push(format!("    {note}"));
    }
    append_warning_line(&mut lines, output);
    append_viewer_status_line(&mut lines, output, status);
    append_structured_command_context(&mut lines, output);
    lines
}

fn append_follow_up_capture_lines(lines: &mut Vec<String>, output: &serde_json::Value, rendered_output: Option<&str>) {
    for hint in crate::agent::runloop::tool_output::tool_follow_up_hints_for_capture(output, rendered_output) {
        lines.push(format!("    {hint}"));
    }
}

async fn render_tool_output_common(
    renderer: &mut AnsiRenderer,
    handle: &InlineHandle,
    name: &str,
    args_val: &serde_json::Value,
    output: &serde_json::Value,
    command_success: bool,
    vt_config: Option<&VTCodeConfig>,
    workspace_root: Option<&Path>,
) -> Result<()> {
    let inline_run_tool = renderer.supports_inline_ui() && streams_pty_output(name, args_val);
    let git_diff_payload = is_git_diff_payload(output);
    let status = ToolDisplayStatus::from_command_output(output, command_success);
    let has_spool_path = output.get("spool_path").is_some();
    let complete_capture = if renderer.supports_inline_ui()
        && is_command_output_call(name, args_val)
        && (inline_run_tool || has_spool_path)
    {
        load_complete_output(output, workspace_root).await
    } else {
        None
    };

    // For streamed inline PTY tools, retain the complete capture separately.
    // Expanded mode renders a bounded live block; compact mode waits for the
    // completion row so the transcript does not jump through transient PTY
    // replacements.
    let inline_pty_command = inline_run_tool && is_command_output_call(name, args_val);
    let compact_pty_without_live_preview =
        inline_pty_command && renderer.tool_display_mode() == ToolDisplayMode::Compact;
    if inline_pty_command && !git_diff_payload {
        // Prefer the complete PTY spool (or the complete inline result) for
        // the session-local tool-output viewer. The live PTY block, when
        // enabled, remains bounded separately.
        let viewer_id = if let Some(capture) = complete_capture.as_deref() {
            let mut viewer_lines =
                build_merged_command_output_lines(name, args_val, capture, workspace_root, output, status);
            append_capture_status_line(&mut viewer_lines, output, complete_capture.as_deref());
            append_follow_up_capture_lines(&mut viewer_lines, output, Some(capture));
            Some(handle.record_tool_output(viewer_lines))
        } else {
            // A rejected or unavailable spool must not fall back to a
            // potentially untrusted path. Keep the command call visible in
            // the viewer while retaining fail-closed spool handling.
            let mut viewer_lines = if has_spool_path {
                build_merged_command_output_lines(name, args_val, "", workspace_root, output, status)
            } else {
                build_pipe_command_output_lines(name, args_val, output, workspace_root, status)
            };
            append_capture_status_line(&mut viewer_lines, output, complete_capture.as_deref());
            append_follow_up_capture_lines(&mut viewer_lines, output, complete_capture.as_deref());
            Some(handle.record_tool_output(viewer_lines))
        };

        let compact_success = renderer.tool_display_mode() == ToolDisplayMode::Compact
            && is_command_output_call(name, args_val)
            && matches!(status, ToolDisplayStatus::Success)
            && !has_compact_command_artefact(output, complete_capture.as_deref());
        if compact_success {
            renderer.collapse_pty_block_to_compact_activity(
                compact_command_text(name, args_val, workspace_root),
                compact_hidden_line_count(output, complete_capture.as_deref()),
                None,
                viewer_id,
            )?;
            return Ok(());
        }

        // A completed PTY result with a warning, diagnostic, diff, or failed
        // capture is a hard boundary regardless of whether a live preview was
        // shown. The next successful command must start a new group.
        renderer.flush_compact_command_group();

        if complete_capture_unavailable(output, complete_capture.as_deref()) {
            renderer.line(MessageStyle::Warning, "Complete command output capture unavailable.")?;
        }
        if let Some(message) = warning_message(output) {
            renderer.line(MessageStyle::Warning, &format!("⚠ {message}"))?;
        }

        // Expanded mode retains the existing live PTY block and makes the
        // post-execution summary available to the normal transcript path.
        // Compact mode has no live block, so render an anchored summary now
        // to keep attention-worthy results identifiable in the inline UI.
        if compact_pty_without_live_preview {
            render_command_summary(renderer, name, args_val, output, command_success, workspace_root, viewer_id, true)?;
        } else {
            record_summary_line(name, args_val, output, command_success);
        }

        if let Some(note) = output_text(output, "critical_note") {
            renderer.line(MessageStyle::ToolError, note)?;
            transcript::append(note);
        }

        // A distinct stderr field is not part of the live PTY block. Keep it
        // visible after completion, while alias detection avoids repeating a
        // stderr stream already included in the terminal capture.
        if let Some(stderr) = stderr_for_inline_display(output) {
            let stderr_lines = normalize_terminal_output_lines(stderr);
            if !stderr_lines.is_empty() {
                renderer.line(MessageStyle::ToolError, &format!("stderr: {}", stderr_lines.join("\n")))?;
            }
        }

        if !has_renderable_stream_content(output) && matches!(status, ToolDisplayStatus::Success) {
            if renderer.tool_display_mode() != ToolDisplayMode::Compact {
                renderer.line(MessageStyle::Info, "(no output)")?;
            }
            return Ok(());
        }

        // Send completion as a status line only when the command needs
        // attention; on success the coloured header bullet is sufficient.
        if !matches!(status, ToolDisplayStatus::Success) {
            if let Some(completion) = compact_run_completion_line(output, status) {
                let indented = format!("    {}", completion);
                renderer.line(MessageStyle::Status, &indented)?;
                transcript::append(&completion);
            }
        }
        return Ok(());
    }

    let viewer_id = if renderer.supports_inline_ui() && is_command_output_call(name, args_val) {
        let mut viewer_lines = if inline_run_tool || has_spool_path {
            complete_capture.as_deref().map_or_else(
                || build_merged_command_output_lines(name, args_val, "", workspace_root, output, status),
                |capture| build_merged_command_output_lines(name, args_val, capture, workspace_root, output, status),
            )
        } else {
            build_pipe_command_output_lines(name, args_val, output, workspace_root, status)
        };
        append_capture_status_line(&mut viewer_lines, output, complete_capture.as_deref());
        append_follow_up_capture_lines(&mut viewer_lines, output, complete_capture.as_deref());
        Some(handle.record_tool_output(viewer_lines))
    } else {
        None
    };

    let compact_command = renderer.supports_inline_ui()
        && is_command_output_call(name, args_val)
        && renderer.tool_display_mode() == ToolDisplayMode::Compact
        && matches!(status, ToolDisplayStatus::Success)
        && !git_diff_payload;
    let compact_file_diff = renderer.supports_inline_ui()
        && renderer.tool_display_mode() == ToolDisplayMode::Compact
        && matches!(status, ToolDisplayStatus::Success)
        && crate::agent::runloop::unified::tool_summary::is_file_modification_tool(name, args_val)
        && has_file_operation_diff(output);
    let compact_artefact = has_compact_command_artefact(output, complete_capture.as_deref());
    if !matches!(status, ToolDisplayStatus::Success) {
        // Warnings and failures are hard boundaries even for command aliases
        // that do not use the live PTY path (for example, `bash`).
        renderer.flush_compact_command_group();
    }
    if git_diff_payload || compact_command && compact_artefact {
        // Attention-worthy output is a hard boundary: do not let a command
        // with visible diagnostics or a diff merge into the preceding group.
        renderer.flush_compact_command_group();
    }
    if compact_file_diff {
        // File changes are glanceable activity, not command-group members.
        // Flush before rendering the file heading so a preceding command row
        // cannot absorb it and the following command starts a fresh group.
        renderer.flush_compact_command_group();
    }
    if compact_command {
        renderer.render_compact_command_activity(
            compact_command_text(name, args_val, workspace_root),
            compact_hidden_line_count(output, complete_capture.as_deref()),
            None,
            viewer_id,
        )?;
        if !compact_artefact {
            return Ok(());
        }
    }

    // Streamed PTY tools with a diff retain the existing live summary in
    // expanded mode. Compact mode suppresses that live row, so render an
    // anchored summary before the diff body instead.
    let skip_live_pty_summary = inline_run_tool && git_diff_payload && !compact_pty_without_live_preview;
    if !(compact_command || skip_live_pty_summary || compact_file_diff) {
        render_command_summary(
            renderer,
            name,
            args_val,
            output,
            command_success,
            workspace_root,
            viewer_id,
            !matches!(status, ToolDisplayStatus::Success) || git_diff_payload,
        )?;
    }

    if complete_capture_unavailable(output, complete_capture.as_deref()) {
        renderer.line(MessageStyle::Warning, "Complete command output capture unavailable.")?;
    }
    if let Some(message) = warning_message(output) {
        renderer.line(MessageStyle::Warning, &format!("⚠ {message}"))?;
    }

    let result = crate::agent::runloop::tool_output::render_tool_output(renderer, Some(name), output, vt_config).await;
    if result.is_ok() && compact_command && compact_artefact {
        render_structured_command_context(renderer, output)?;
    }
    if !matches!(status, ToolDisplayStatus::Success) {
        // The warning/failure row itself is visible, but it must not remain
        // the active tail that a later successful command could extend.
        renderer.flush_compact_command_group();
    }
    if compact_command && compact_artefact {
        // Some attention-worthy metadata (for example, a critical note) can
        // be rendered without emitting another line. End the active compact
        // tail explicitly so the next command cannot merge into this row.
        renderer.flush_compact_command_group();
    }
    result
}

fn render_error_common(renderer: &mut AnsiRenderer, name: &str, error: &str, error_type: &str) -> Result<()> {
    let err_msg = format!("Tool '{name}' {error_type}: {error}");
    renderer.line(MessageStyle::Error, &err_msg)?;
    Ok(())
}

#[derive(Default)]
struct OutcomeState {
    turn_modified_files: Vec<PathBuf>,
    last_tool_stdout: Option<String>,
}

impl OutcomeState {
    fn into_tuple(self) -> (Vec<PathBuf>, Option<String>) {
        (self.turn_modified_files, self.last_tool_stdout)
    }
}

struct OutcomeContext<'a> {
    session_stats: &'a mut SessionStats,
    renderer: &'a mut AnsiRenderer,
    handle: &'a InlineHandle,
    harness_state: &'a mut crate::agent::runloop::unified::run_loop_context::HarnessTurnState,
    mcp_panel_state: &'a mut McpPanelState,
    vt_config: Option<&'a VTCodeConfig>,
    workspace_root: Option<&'a Path>,
}

struct SuccessPayload<'a> {
    output: &'a serde_json::Value,
    stdout: &'a Option<String>,
    modified_files: &'a [String],
    command_success: bool,
}

async fn handle_success_common(
    ctx: &mut OutcomeContext<'_>,
    name: &str,
    args_val: &serde_json::Value,
    payload: SuccessPayload<'_>,
    state: &mut OutcomeState,
) -> Result<()> {
    ctx.session_stats.record_tool(name);

    if let Some(tool_name) = name.strip_prefix("mcp_") {
        ctx.renderer.flush_compact_command_group();
        let tool_name = tool_name.trim_start_matches('_');
        let tool_name = tool_name.split("__").last().unwrap_or(tool_name);
        record_mcp_outcome_event(ctx.mcp_panel_state, tool_name, args_val, payload.command_success);
    } else if is_task_tracker_tool(name) && ctx.renderer.supports_inline_ui() {
        ctx.renderer.flush_compact_command_group();
        let block_lines = task_tracker_block_lines(payload.output);
        if !block_lines.is_empty() {
            ctx.handle.update_task_panel_with_metadata(
                block_lines.clone(),
                crate::agent::runloop::tool_output::tracker_panel_metadata(payload.output),
            );
            apply_task_tracker_block(ctx.handle, ctx.harness_state, block_lines);
        }
    } else {
        render_tool_output_common(
            ctx.renderer,
            ctx.handle,
            name,
            args_val,
            payload.output,
            payload.command_success,
            ctx.vt_config,
            ctx.workspace_root,
        )
        .await?;
    }

    state.last_tool_stdout = if payload.command_success {
        payload.stdout.clone()
    } else {
        None
    };

    if !payload.modified_files.is_empty() {
        state.turn_modified_files.extend(collect_modified_files(payload.modified_files));
    }

    Ok(())
}

fn handle_non_success_common(
    ctx: &mut OutcomeContext<'_>,
    name: &str,
    args_val: &serde_json::Value,
    status: &ToolExecutionStatus,
) -> Result<()> {
    ctx.renderer.flush_compact_command_group();

    // Expanded PTY tools already rendered "• Ran ..." in the pre-execution
    // inline block. Compact PTY tools suppress that block, so retain the
    // command summary for failures and cancellations instead of relying on a
    // header that was never emitted.
    let has_live_pty_preview = ctx.renderer.supports_inline_ui()
        && is_run_pty_tool(name, args_val)
        && ctx.renderer.tool_display_mode() != ToolDisplayMode::Compact;

    match status {
        ToolExecutionStatus::Failure { error } | ToolExecutionStatus::Timeout { error } => {
            let user_message = error.user_message();
            let viewer_id = if ctx.renderer.supports_inline_ui() && is_command_output_call(name, args_val) {
                Some(ctx.handle.record_tool_output(vec![
                    command_output_header(name, args_val, ctx.workspace_root),
                    format!(
                        "    {}: {}",
                        if matches!(status, ToolExecutionStatus::Timeout { .. }) {
                            "timed out"
                        } else {
                            "failed"
                        },
                        user_message
                    ),
                ]))
            } else {
                None
            };
            if !has_live_pty_preview {
                if let Some(viewer_id) = viewer_id {
                    ctx.renderer.set_next_tool_output_anchor(viewer_id);
                }
                render_non_success_summary(
                    ctx.renderer,
                    name,
                    args_val,
                    Some("error"),
                    ctx.workspace_root,
                    ToolDisplayStatus::Failure,
                )?;
            }
            render_error_common(
                ctx.renderer,
                name,
                &user_message,
                if matches!(status, ToolExecutionStatus::Timeout { .. }) {
                    "timed out"
                } else {
                    "failure"
                },
            )?;
        }
        ToolExecutionStatus::Cancelled => {
            let viewer_id = if ctx.renderer.supports_inline_ui() && is_command_output_call(name, args_val) {
                Some(ctx.handle.record_tool_output(vec![
                    command_output_header(name, args_val, ctx.workspace_root),
                    "    warning: tool execution cancelled".to_string(),
                ]))
            } else {
                None
            };
            if !has_live_pty_preview {
                if let Some(viewer_id) = viewer_id {
                    ctx.renderer.set_next_tool_output_anchor(viewer_id);
                }
                render_non_success_summary(
                    ctx.renderer,
                    name,
                    args_val,
                    Some("cancelled"),
                    ctx.workspace_root,
                    ToolDisplayStatus::Warning,
                )?;
            }
            ctx.renderer.line(MessageStyle::Info, "Tool execution cancelled")?;
        }
        ToolExecutionStatus::Success { .. } => {}
    };

    Ok(())
}

fn render_non_success_summary(
    renderer: &mut AnsiRenderer,
    name: &str,
    args_val: &serde_json::Value,
    stream_label: Option<&str>,
    workspace_root: Option<&Path>,
    status: ToolDisplayStatus,
) -> Result<()> {
    let summary_ctx = crate::agent::runloop::unified::tool_summary::ToolSummaryRenderContext { workspace_root };
    crate::agent::runloop::unified::tool_summary::render_expanded_tool_call_summary(
        renderer,
        name,
        args_val,
        stream_label,
        &summary_ctx,
        status.colour(ColourPalette::default()),
    )
}

async fn process_outcome_common(
    ctx: &mut OutcomeContext<'_>,
    name: &str,
    args_val: &serde_json::Value,
    outcome: &ToolPipelineOutcome,
) -> Result<OutcomeState> {
    let mut state = OutcomeState::default();

    match &outcome.status {
        ToolExecutionStatus::Success {
            output, stdout, modified_files, command_success, ..
        } => {
            handle_success_common(
                ctx,
                name,
                args_val,
                SuccessPayload {
                    output,
                    stdout,
                    modified_files,
                    command_success: *command_success,
                },
                &mut state,
            )
            .await?;
        }
        _ => handle_non_success_common(ctx, name, args_val, &outcome.status)?,
    }

    Ok(state)
}

pub(crate) async fn handle_pipeline_output(
    ctx: &mut RunLoopContext<'_>,
    name: &str,
    args_val: &serde_json::Value,
    outcome: &ToolPipelineOutcome,
    vt_config: Option<&VTCodeConfig>,
) -> Result<(Vec<PathBuf>, Option<String>)> {
    // The registry owns the workspace used by the executor and the spooler.
    // Use it here even on the Copilot path, whose lightweight run-loop
    // context intentionally does not carry an auto-permission context.
    let workspace_root = Some(ctx.tool_registry.workspace_root().as_path());
    let mut output_ctx = OutcomeContext {
        session_stats: ctx.session_stats,
        renderer: ctx.renderer,
        handle: ctx.handle,
        harness_state: ctx.harness_state,
        mcp_panel_state: ctx.mcp_panel_state,
        vt_config,
        workspace_root,
    };
    let state = process_outcome_common(&mut output_ctx, name, args_val, outcome).await?;
    Ok(state.into_tuple())
}

// Adapter for TurnLoopContext (to avoid duplication when handling tool output in the turn loop)
pub(crate) async fn handle_pipeline_output_from_turn_ctx(
    ctx: &mut crate::agent::runloop::unified::turn::TurnLoopContext<'_>,
    name: &str,
    args_val: &serde_json::Value,
    outcome: &ToolPipelineOutcome,
    vt_config: Option<&VTCodeConfig>,
) -> Result<(Vec<PathBuf>, Option<String>)> {
    let mut run_ctx = ctx.as_run_loop_context();
    let (modified_files, last_stdout) =
        handle_pipeline_output(&mut run_ctx, name, args_val, outcome, vt_config).await?;

    if let ToolExecutionStatus::Success { output, modified_files, command_success: true, .. } = &outcome.status {
        let activity_paths =
            collect_instruction_activity_paths(ctx.config.workspace.as_path(), args_val, output, modified_files);
        if !activity_paths.is_empty() {
            ctx.context_manager.record_instruction_activity_paths(activity_paths);
        }
    }

    Ok((modified_files, last_stdout))
}

/// Tests for tool-output rendering, capture, grouping, and result accounting.
#[cfg(test)]
#[path = "tool_output_handler_tests/mod.rs"]
mod tests;
