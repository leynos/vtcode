//! Canonical stream selection and merged-output normalization regressions.

use super::*;

#[test]
fn ordered_stream_texts_deduplicates_merged_output_aliases() {
    let output = serde_json::json!({
        "output": "stdout line\nstderr line",
        "stdout": "stdout line",
        "stderr": "stderr line"
    });

    assert_eq!(ordered_stream_texts(&output), vec!["stdout line\nstderr line"]);
}

#[test]
fn ordered_stream_texts_preserves_distinct_pipe_streams() {
    let output = serde_json::json!({
        "output": "merged line",
        "stdout": "stdout line",
        "stderr": "stderr line"
    });

    assert_eq!(ordered_stream_texts(&output), vec!["merged line", "stdout line", "stderr line"]);
}

#[test]
fn canonical_pipe_streams_preserve_unrepresented_content() {
    let output = serde_json::json!({
        "stdout": "command output",
        "content": "additional structured content"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![
            (Some("stdout"), "command output"),
            (None, "additional structured content")
        ]
    );
}

#[test]
fn canonical_pipe_streams_keep_merged_output_once() {
    let output = serde_json::json!({
        "output": "stdout line\nstderr line",
        "stdout": "stdout line",
        "stderr": "stderr line"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].label, None);
    assert_eq!(streams[0].text, "stdout line\nstderr line");
}

#[test]
fn canonical_pipe_streams_label_separate_streams() {
    let output = serde_json::json!({
        "stdout": "stdout line",
        "stderr": "stderr line"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![(Some("stdout"), "stdout line"), (Some("stderr"), "stderr line")]
    );
}

#[test]
fn canonical_pipe_streams_preserve_identical_named_streams() {
    let output = serde_json::json!({
        "stdout": "same output",
        "stderr": "same output"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![(Some("stdout"), "same output"), (Some("stderr"), "same output")]
    );
}

#[test]
fn canonical_pipe_streams_require_distinct_merged_occurrences() {
    let single_occurrence = serde_json::json!({
        "output": "same output",
        "stdout": "same output",
        "stderr": "same output"
    });
    let streams = canonical_pipe_streams(&single_occurrence);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![(Some("stdout"), "same output"), (Some("stderr"), "same output")]
    );
    assert_eq!(stderr_for_inline_display(&single_occurrence), Some("same output"));

    let distinct_occurrences = serde_json::json!({
        "output": "same output\nsame output",
        "stdout": "same output",
        "stderr": "same output"
    });
    let streams = canonical_pipe_streams(&distinct_occurrences);
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0].label, None);
    assert_eq!(streams[0].text, "same output\nsame output");
    assert_eq!(stderr_for_inline_display(&distinct_occurrences), None);
}

#[test]
fn canonical_pipe_streams_preserve_merged_lines_when_named_streams_overlap() {
    let output = serde_json::json!({
        "output": "same output\nmerged-only output",
        "stdout": "same output",
        "stderr": "same output"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![
            (Some("stdout"), "same output"),
            (Some("stderr"), "same output"),
            (None, "same output\nmerged-only output")
        ]
    );
}

#[test]
fn canonical_pipe_streams_preserve_full_named_alias() {
    let output = serde_json::json!({
        "output": "stdout line",
        "stdout": "stdout line\nsecond stdout line",
        "stderr": "stderr line"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![
            (Some("stdout"), "stdout line\nsecond stdout line"),
            (Some("stderr"), "stderr line")
        ]
    );
}

#[test]
fn canonical_pipe_streams_preserve_distinct_streams_when_output_is_preview() {
    let output = serde_json::json!({
        "output": "preview",
        "stdout": "preview\nstdout line",
        "stderr": "preview\nstderr line"
    });

    let streams = canonical_pipe_streams(&output);
    assert_eq!(
        streams.iter().map(|stream| (stream.label, stream.text)).collect::<Vec<_>>(),
        vec![
            (Some("stdout"), "preview\nstdout line"),
            (Some("stderr"), "preview\nstderr line")
        ]
    );
}

#[test]
fn normalize_terminal_output_lines_handles_ansi_rewrites_and_blanks() {
    let capture = "stale\n\x1b[2J\x1b[H\x1b[31mred\x1b[0m\rfinal\n\nlast\n";

    assert_eq!(normalize_terminal_output_lines(capture), vec!["final", "", "last"]);
    assert_eq!(normalize_terminal_output_lines("abc\x08d\n"), vec!["abd"]);
}

#[test]
fn build_pipe_command_output_lines_labels_stderr_once() {
    let output = serde_json::json!({
        "stdout": "normal output",
        "stderr": "diagnostic output",
        "exit_code": 1
    });

    assert_eq!(
        build_pipe_command_output_lines(
            tools::EXECUTE_CODE,
            &serde_json::json!({"command": "printf test"}),
            &output,
            None,
            ToolDisplayStatus::Failure,
        ),
        vec![
            "• Ran printf test",
            "  stdout:",
            "    normal output",
            "  stderr:",
            "    diagnostic output",
            "    ✗ run error, exit code: 1",
        ]
    );
}

#[test]
fn build_merged_command_output_lines_keeps_complete_capture_and_status_once() {
    let output = serde_json::json!({
        "exit_code": 2,
        "critical_note": "output was retained in the current session"
    });
    let capture = "stdout line\nstderr line\n";

    let lines = build_merged_command_output_lines(
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "long command"}),
        capture,
        None,
        &output,
        ToolDisplayStatus::Failure,
    );

    assert_eq!(lines[0], "• Ran long command");
    assert!(lines.contains(&"  └ stdout line".to_string()));
    assert!(lines.contains(&"    stderr line".to_string()));
    assert!(lines.contains(&"    output was retained in the current session".to_string()));
    assert_eq!(lines.iter().filter(|line| line.contains("stderr line")).count(), 1);
    assert_eq!(lines.iter().filter(|line| line.contains("exit code: 2")).count(), 1);
}

#[test]
fn build_merged_command_output_lines_keeps_distinct_stderr_without_capture() {
    let output = serde_json::json!({"stderr": "diagnostic output"});

    let lines = build_merged_command_output_lines(
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "long command"}),
        "",
        None,
        &output,
        ToolDisplayStatus::Success,
    );

    assert_eq!(lines, vec!["• Ran long command", "  stderr:", "    diagnostic output",]);
}

#[test]
fn build_merged_command_output_lines_labels_distinct_stderr_with_merged_output() {
    let output = serde_json::json!({
        "output": "normal output",
        "stderr": "diagnostic output"
    });

    let lines = build_merged_command_output_lines(
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "long command"}),
        "normal output\ndiagnostic output\n",
        None,
        &output,
        ToolDisplayStatus::Success,
    );

    assert_eq!(
        lines,
        vec![
            "• Ran long command",
            "  └ normal output",
            "  stderr:",
            "    diagnostic output",
        ]
    );
}

#[test]
fn build_merged_command_output_lines_labels_identical_named_streams_without_merged_output() {
    let output = serde_json::json!({
        "stdout": "same output",
        "stderr": "same output"
    });

    let lines = build_merged_command_output_lines(
        tools::RUN_PTY_CMD,
        &serde_json::json!({"command": "long command"}),
        "same output\nsame output",
        None,
        &output,
        ToolDisplayStatus::Success,
    );

    assert_eq!(
        lines,
        vec![
            "• Ran long command",
            "  stdout:",
            "    same output",
            "  stderr:",
            "    same output",
        ]
    );
}
