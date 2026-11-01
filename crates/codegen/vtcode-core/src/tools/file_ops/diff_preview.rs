//! Diff preview utilities for file operations.

use crate::config::constants::diff;
use crate::utils::diff::{DiffOptions, compute_diff_with_theme};
use serde_json::{Value, json};
use std::time::Instant;
use vtcode_commons::ansi::strip_ansi;
use vtcode_commons::diff_preview::count_diff_changes;

/// Create a diff preview response when content exceeds the size limit.
pub fn diff_preview_size_skip() -> Value {
    json!({
        "skipped": true,
        "reason": "content_exceeds_preview_limit",
        "max_bytes": diff::MAX_PREVIEW_BYTES
    })
}

/// Create a diff preview response when inline diffs are suppressed due to too many changes.
pub fn diff_preview_suppressed(additions: usize, deletions: usize, line_count: usize) -> Value {
    json!({
        "skipped": true,
        "suppressed": true,
        "reason": "too_many_changes",
        "message": diff::SUPPRESSION_MESSAGE,
        "summary": {
            "additions": additions,
            "deletions": deletions,
            "total_lines": line_count
        }
    })
}

/// Create a diff preview response when an error prevents diff generation.
pub fn diff_preview_error_skip(reason: &str, detail: Option<&str>) -> Value {
    match detail {
        Some(value) => json!({
            "skipped": true,
            "reason": reason,
            "detail": value
        }),
        None => json!({
            "skipped": true,
            "reason": reason
        }),
    }
}

/// Return the canonical bounded diff entries from a file-operation response.
///
/// `diff[]` is the shared response contract for multi-file operations. The
/// legacy `diff_preview` object is normalized into a one-entry list so older
/// write/create responses render and persist through the same path.
pub fn canonical_diff_previews(output: &Value) -> Vec<Value> {
    if let Some(diffs) = output.get("diff").and_then(Value::as_array) {
        return diffs.clone();
    }

    let Some(preview) = output.get("diff_preview") else {
        return Vec::new();
    };

    let mut entry = preview.clone();
    if let Some(fields) = entry.as_object_mut() {
        if !fields.contains_key("path")
            && let Some(path) = output.get("path").and_then(Value::as_str)
        {
            fields.insert("path".to_string(), Value::String(path.to_string()));
        }
        if !fields.contains_key("operation") {
            let operation = if output.get("created").and_then(Value::as_bool) == Some(true)
                || output.get("file_existed").and_then(Value::as_bool) == Some(false)
            {
                "created"
            } else {
                "updated"
            };
            fields.insert("operation".to_string(), Value::String(operation.to_string()));
        }
    }

    vec![entry]
}

/// Determine whether a canonical diff contract represents an effective
/// mutation. `None` means the response is not a file-operation response and
/// callers should use the tool's normal success semantics.
pub fn diff_output_has_effective_change(output: &Value) -> Option<bool> {
    if output.get("skipped").and_then(Value::as_bool) == Some(true)
        || output.get("conflict").and_then(Value::as_bool) == Some(true)
        || output.get("success").and_then(Value::as_bool) == Some(false)
    {
        return Some(false);
    }

    if output.get("diff").is_none() && output.get("diff_preview").is_none() {
        return None;
    }

    Some(canonical_diff_previews(output).iter().any(diff_preview_has_effective_change))
}

fn diff_preview_has_effective_change(preview: &Value) -> bool {
    if preview.get("is_empty").and_then(Value::as_bool) == Some(true) {
        return matches!(preview.get("operation").and_then(Value::as_str), Some("created" | "deleted"));
    }

    if preview
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|content| !content.is_empty())
    {
        return true;
    }

    // A skipped preview still represents a completed mutation when the
    // operation itself succeeded; only an explicit `is_empty` preview is a
    // no-op. This covers bounded/suppressed previews without over-counting
    // writes that were skipped or conflicted at the response level.
    preview.get("skipped").and_then(Value::as_bool) == Some(true)
        || preview
            .get("summary")
            .and_then(|summary| summary.get("additions"))
            .and_then(Value::as_u64)
            .is_some_and(|additions| additions > 0)
        || preview
            .get("summary")
            .and_then(|summary| summary.get("deletions"))
            .and_then(Value::as_u64)
            .is_some_and(|deletions| deletions > 0)
}

/// Build a unified diff preview between before and after content.
pub fn build_diff_preview(path: &str, before: Option<&str>, after: &str) -> Value {
    let started = Instant::now();
    let previous = before.unwrap_or("");
    let old_label = format!("a/{path}");
    let new_label = format!("b/{path}");

    let diff_bundle = compute_diff_with_theme(
        previous,
        after,
        DiffOptions {
            context_lines: diff::CONTEXT_RADIUS,
            old_label: Some(old_label.as_str()),
            new_label: Some(new_label.as_str()),
            missing_newline_hint: true,
        },
    );
    // Tool responses carry a plain unified diff. The terminal/UI renderers
    // apply colours, gutters, and syntax highlighting after parsing the diff;
    // embedding ANSI here would hide hunk markers and line prefixes from that
    // parser and make apply_patch previews fall back to raw text.
    let formatted = strip_ansi(&diff_bundle.formatted);

    if formatted.trim().is_empty() {
        tracing::debug!(
            target: "vtcode.tools.diff",
            path,
            before_bytes = previous.len(),
            after_bytes = after.len(),
            additions = 0,
            deletions = 0,
            line_count = 0,
            truncated = false,
            suppressed = false,
            elapsed_ms = started.elapsed().as_millis(),
            "diff preview generated"
        );

        return json!({
            "content": "",
            "truncated": false,
            "omitted_line_count": 0,
            "skipped": false,
            "is_empty": true,
            "additions": 0,
            "deletions": 0
        });
    }

    let line_count = formatted.lines().count();
    let counts = count_diff_changes(&diff_bundle.hunks);
    let additions = counts.additions;
    let deletions = counts.deletions;
    let total_changes = counts.total();

    if total_changes > diff::MAX_SINGLE_FILE_CHANGES {
        tracing::debug!(
            target: "vtcode.tools.diff",
            path,
            before_bytes = previous.len(),
            after_bytes = after.len(),
            additions,
            deletions,
            line_count,
            truncated = false,
            suppressed = true,
            elapsed_ms = started.elapsed().as_millis(),
            "diff preview suppressed (too many changes)"
        );

        return diff_preview_suppressed(additions, deletions, line_count);
    }

    if line_count > diff::MAX_PREVIEW_LINES {
        let lines: Vec<&str> = formatted.lines().collect();
        let head_count = diff::HEAD_LINE_COUNT.min(lines.len());
        let tail_count = diff::TAIL_LINE_COUNT.min(lines.len().saturating_sub(head_count));
        let omitted = lines.len().saturating_sub(head_count + tail_count);

        let mut condensed = Vec::with_capacity(head_count + tail_count + 1);
        condensed.extend(lines[..head_count].iter().copied());
        if omitted > 0 {
            condensed.push("");
        }
        if tail_count > 0 {
            let tail_start = lines.len().saturating_sub(tail_count);
            condensed.extend(lines[tail_start..].iter().copied());
        }

        let diff_output = if omitted > 0 {
            let mut result = condensed[..head_count].join("\n");
            result.push_str(&format!("\n... {omitted} lines omitted ...\n"));
            result.push_str(&condensed[head_count + 1..].join("\n"));
            result
        } else {
            condensed.join("\n")
        };

        let elapsed = started.elapsed().as_millis();

        tracing::debug!(
            target: "vtcode.tools.diff",
            path,
            before_bytes = previous.len(),
            after_bytes = after.len(),
            additions,
            deletions,
            line_count,
            omitted_lines = omitted,
            truncated = true,
            suppressed = false,
            elapsed_ms = elapsed,
            "diff preview generated"
        );

        json!({
            "content": diff_output,
            "truncated": true,
            "omitted_line_count": omitted,
            "skipped": false,
            "additions": additions,
            "deletions": deletions
        })
    } else {
        let elapsed = started.elapsed().as_millis();

        tracing::debug!(
            target: "vtcode.tools.diff",
            path,
            before_bytes = previous.len(),
            after_bytes = after.len(),
            additions,
            deletions,
            line_count,
            truncated = false,
            suppressed = false,
            elapsed_ms = elapsed,
            "diff preview generated"
        );

        json!({
            "content": formatted,
            "truncated": false,
            "omitted_line_count": 0,
            "skipped": false,
            "additions": additions,
            "deletions": deletions
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use vtcode_commons::ansi::strip_ansi;

    use super::*;

    #[test]
    fn canonical_diff_previews_normalize_legacy_write_output() {
        let previews = canonical_diff_previews(&json!({
            "path": "README.md",
            "file_existed": true,
            "diff_preview": {"content": "diff", "skipped": false}
        }));

        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0]["path"], "README.md");
        assert_eq!(previews[0]["operation"], "updated");
    }

    #[test]
    fn effective_change_distinguishes_noop_skipped_and_empty_file_operations() {
        assert_eq!(
            diff_output_has_effective_change(&json!({
                "success": true,
                "diff": [{"operation": "updated", "is_empty": true}]
            })),
            Some(false)
        );
        assert_eq!(
            diff_output_has_effective_change(&json!({"success": true, "skipped": true, "diff": []})),
            Some(false)
        );
        assert_eq!(
            diff_output_has_effective_change(&json!({
                "success": true,
                "diff": [{"operation": "created", "is_empty": true}]
            })),
            Some(true)
        );
    }

    #[test]
    fn build_diff_preview_keeps_serialized_content_plain_for_ui_parsing() {
        let preview = build_diff_preview("README.md", Some("before\n"), "after\n");
        let content = preview
            .get("content")
            .and_then(Value::as_str)
            .expect("changed preview should contain diff content");

        assert_eq!(content, strip_ansi(content));
        assert!(content.contains("-before"));
        assert!(content.contains("+after"));
    }
}
