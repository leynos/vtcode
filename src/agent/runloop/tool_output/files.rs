use anyhow::Result;
use serde_json::Value;
use vtcode_commons::preview;
use vtcode_core::config::constants::tools;
use vtcode_core::config::{ToolDisplayMode, ToolOutputMode};
use vtcode_core::utils::ansi::{AnsiRenderer, MessageStyle};

use super::render_tree_detail;
use super::streams::{render_diff_content_block, strip_ansi_codes};
use super::styles::{GitStyles, LsStyles};
pub(crate) use vtcode_commons::diff_preview::format_numbered_unified_diff as format_diff_content_lines_with_numbers;
use vtcode_core::tools::file_ops::canonical_diff_previews;

/// Constants for line and content limits (compact display)
const MAX_DISPLAYED_FILES: usize = 100; // Limit displayed files to reduce clutter

/// Helper to extract optional string from JSON value
fn get_string<'a>(val: &'a Value, key: &str) -> Option<&'a str> {
    val.get(key).and_then(|v| v.as_str())
}

/// Helper to extract optional boolean from JSON value
fn get_bool(val: &Value, key: &str) -> bool {
    val.get(key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Helper to extract optional u64 from JSON value
fn get_u64(val: &Value, key: &str) -> Option<u64> {
    val.get(key).and_then(|v| v.as_u64())
}

fn compact_file_glance_enabled(renderer: &AnsiRenderer) -> bool {
    renderer.supports_inline_ui() && renderer.tool_display_mode() == ToolDisplayMode::Compact
}

fn render_file_heading(renderer: &mut AnsiRenderer, heading: &str) -> Result<()> {
    if compact_file_glance_enabled(renderer) {
        renderer.line(MessageStyle::Info, &format!("• {heading}"))
    } else {
        renderer.line(MessageStyle::ToolDetail, heading)
    }
}

pub(crate) fn render_write_file_preview(
    renderer: &mut AnsiRenderer,
    payload: &Value,
    git_styles: &GitStyles,
    ls_styles: &LsStyles,
) -> Result<()> {
    let diffs = canonical_diff_previews(payload);

    // Show basic metadata (compact format)
    if get_bool(payload, "created") && diffs.is_empty() {
        if compact_file_glance_enabled(renderer) {
            let heading = get_string(payload, "path")
                .map_or_else(|| "File created".to_string(), |path| format!("Created {path}"));
            render_file_heading(renderer, &heading)?;
        } else {
            renderer.line(MessageStyle::ToolDetail, "File created")?;
        }
    }

    if let Some(encoding) = get_string(payload, "encoding") {
        renderer.line(MessageStyle::ToolDetail, &format!("encoding: {encoding}"))?;
    }

    if diffs.is_empty() {
        if get_bool(payload, "skipped") {
            let reason = get_string(payload, "reason").unwrap_or("already exists");
            renderer.line(MessageStyle::ToolDetail, &format!("write skipped: {reason}"))?;
        } else if get_bool(payload, "conflict") {
            renderer.line(MessageStyle::ToolDetail, "write blocked: file conflict")?;
        } else if let Some(error) = get_string(payload, "error") {
            renderer.line(MessageStyle::ToolDetail, error)?;
        }
        return Ok(());
    }

    render_diff_preview_entries(renderer, &diffs, git_styles, ls_styles)
}

pub(crate) fn render_apply_patch_diff_preview(
    renderer: &mut AnsiRenderer,
    payload: &Value,
    git_styles: &GitStyles,
    ls_styles: &LsStyles,
) -> Result<()> {
    let diffs = canonical_diff_previews(payload);
    if diffs.is_empty() {
        if get_bool(payload, "conflict") {
            renderer.line(MessageStyle::ToolDetail, "patch blocked: file conflict")?;
        } else if let Some(error) = get_string(payload, "error") {
            renderer.line(MessageStyle::ToolDetail, error)?;
        }
        return Ok(());
    }

    render_diff_preview_entries(renderer, &diffs, git_styles, ls_styles)
}

fn render_diff_preview_entries(
    renderer: &mut AnsiRenderer,
    diffs: &[Value],
    git_styles: &GitStyles,
    ls_styles: &LsStyles,
) -> Result<()> {
    for diff in diffs.iter().take(MAX_DISPLAYED_FILES) {
        let path = get_string(diff, "path");
        let operation = get_string(diff, "operation");
        let additions = get_u64(diff, "additions").or_else(|| {
            diff.get("summary")
                .and_then(|summary| summary.get("additions"))
                .and_then(Value::as_u64)
        });
        let deletions = get_u64(diff, "deletions").or_else(|| {
            diff.get("summary")
                .and_then(|summary| summary.get("deletions"))
                .and_then(Value::as_u64)
        });

        let action = match operation {
            Some("created") => "Created",
            Some("deleted") => "Deleted",
            _ => "Edited",
        };
        let mut heading = path.map_or_else(|| format!("{action} file"), |path| format!("{action} {path}"));
        if additions.is_some() || deletions.is_some() {
            heading.push_str(&format!(" (+{} -{})", additions.unwrap_or_default(), deletions.unwrap_or_default()));
        }
        render_file_heading(renderer, &heading)?;

        if get_bool(diff, "skipped") {
            let reason = get_string(diff, "reason").unwrap_or("skipped");
            if let Some(detail) = get_string(diff, "detail") {
                renderer.line(MessageStyle::ToolDetail, &format!("preview: {reason} ({detail})"))?;
            } else {
                renderer.line(MessageStyle::ToolDetail, &format!("preview: {reason}"))?;
            }
            continue;
        }

        let diff_content = get_string(diff, "content").unwrap_or("");
        if diff_content.is_empty() && get_bool(diff, "is_empty") {
            renderer.line(MessageStyle::ToolDetail, "(no changes)")?;
            continue;
        }

        if !diff_content.is_empty() {
            renderer.line(MessageStyle::ToolDetail, "")?;
            render_diff_content(renderer, diff_content, git_styles, ls_styles)?;
        }

        if get_bool(diff, "truncated") {
            if let Some(omitted) = get_u64(diff, "omitted_line_count") {
                renderer.line(
                    MessageStyle::ToolDetail,
                    &format!("… +{omitted} lines (use exec_command with sed for full view)"),
                )?;
            } else {
                renderer.line(MessageStyle::ToolDetail, "… diff truncated")?;
            }
        }
    }

    Ok(())
}

pub(crate) fn render_list_dir_output(renderer: &mut AnsiRenderer, val: &Value, _ls_styles: &LsStyles) -> Result<()> {
    // Get pagination info first
    let count = get_u64(val, "count").unwrap_or(0);
    let total = get_u64(val, "total").unwrap_or(0);
    let page = get_u64(val, "page").unwrap_or(1);
    let _has_more = get_bool(val, "has_more");
    let per_page = get_u64(val, "per_page").unwrap_or(20);

    // Show path - always display root directory for clarity
    if let Some(path) = get_string(val, "path") {
        let display_path = if path.is_empty() { "/" } else { path };
        renderer
            .line(MessageStyle::ToolDetail, &format!("{}{}", display_path, if !path.is_empty() { "/" } else { "" }))?;
    }

    // Show summary - compact format
    if count > 0 || total > 0 {
        let start_idx = (page - 1) * per_page + 1;
        let _end_idx = start_idx + count - 1;

        // Simplified summary without pagination details that confuse the agent
        let summary = if total > count {
            format!("Showing {count} of {total} items")
        } else {
            format!("{count} items total")
        };
        renderer.line(MessageStyle::ToolDetail, &summary)?;
    }

    // Render items grouped by type
    if let Some(items) = val.get("items").and_then(|v| v.as_array()) {
        if items.is_empty() {
            renderer.line(MessageStyle::ToolDetail, "(empty)")?;
        } else {
            let mut directories = Vec::new();
            let mut files = Vec::new();

            // Group items by type
            for item in items.iter().take(MAX_DISPLAYED_FILES) {
                if let Some(name) = get_string(item, "name") {
                    let item_type = get_string(item, "type").unwrap_or("file");
                    let size = get_u64(item, "size");

                    if item_type == "directory" {
                        directories.push((name.to_string(), size));
                    } else {
                        files.push((name.to_string(), size));
                    }
                }
            }

            // Get sort order from the JSON value, defaulting to alphabetical by name
            let sort_order = get_string(val, "sort").unwrap_or("name");

            // Sort each group based on the specified sort order
            match sort_order {
                "size" => {
                    // Sort by size (largest first), with None sizes treated as 0
                    directories.sort_by(|a, b| b.1.unwrap_or(0).cmp(&a.1.unwrap_or(0)));
                    files.sort_by(|a, b| b.1.unwrap_or(0).cmp(&a.1.unwrap_or(0)));
                }
                "name" => {
                    // Sort alphabetically (case-insensitive for natural sorting)
                    directories.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                }
                "type" => {
                    // Sort by type/extension (files with extensions first, then by extension)
                    directories.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                    files.sort_by(|a, b| {
                        let ext_a = std::path::Path::new(&a.0)
                            .extension()
                            .map(|e| e.to_string_lossy().to_lowercase())
                            .unwrap_or_default();
                        let ext_b = std::path::Path::new(&b.0)
                            .extension()
                            .map(|e| e.to_string_lossy().to_lowercase())
                            .unwrap_or_default();

                        ext_a.cmp(&ext_b).then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                    });
                }
                _ => {
                    // Default to alphabetical sorting
                    directories.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                    files.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
                }
            }

            // Calculate max name width for directories (with trailing /) and files
            let max_name_width = if !directories.is_empty() || !files.is_empty() {
                let dir_max_width = directories
                    .iter()
                    .map(|(name, _)| preview::display_width(name) + 1) // +1 for trailing /
                    .max()
                    .unwrap_or(10)
                    .min(40);

                let file_max_width = files
                    .iter()
                    .map(|(name, _)| preview::display_width(name))
                    .max()
                    .unwrap_or(10)
                    .min(40);

                dir_max_width.max(file_max_width)
            } else {
                10 // Default width if no items
            };

            // Render directories first with section header
            if !directories.is_empty() {
                renderer.line(MessageStyle::ToolDetail, "[Directories]")?;
                for (name, _size) in &directories {
                    let name_with_slash = format!("{name}/");
                    let display = preview::pad_to_display_width(&name_with_slash, max_name_width, ' ');
                    renderer.line(MessageStyle::ToolDetail, &display)?;
                }

                // Add visual separation between directories and files
                if !files.is_empty() {
                    renderer.line(MessageStyle::ToolDetail, "")?; // Add blank line
                }
            }

            // Render files with section header
            if !files.is_empty() {
                renderer.line(MessageStyle::ToolDetail, "[Files]")?;
                for (name, _size) in &files {
                    // Simple file name display without size or emoji
                    let display = preview::pad_to_display_width(name, max_name_width, ' ');
                    renderer.line(MessageStyle::ToolDetail, &display)?;
                }
            }

            let omitted = items.len().saturating_sub(MAX_DISPLAYED_FILES);
            if omitted > 0 {
                renderer.line(MessageStyle::ToolDetail, &format!("+ {omitted} more items not shown"))?;
            }
        }
    }

    // Pagination navigation removed - agent should work with first page results
    // If more items exist, agent can call list_files again with specific page parameter

    Ok(())
}

pub(crate) fn render_read_file_output(renderer: &mut AnsiRenderer, val: &Value) -> Result<()> {
    // Batch read: show compact per-file summary
    if let Some(items) = val.get("items").and_then(Value::as_array) {
        let files_read = get_u64(val, "files_read").unwrap_or(items.len() as u64);
        let files_ok = get_u64(val, "files_succeeded").unwrap_or(0);
        let failed = files_read.saturating_sub(files_ok);

        let mut summary = format!("{} file{} read", files_ok, if files_ok == 1 { "" } else { "s" });
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        render_tree_detail(renderer, &summary)?;

        for item in items.iter().take(MAX_BATCH_DISPLAY_FILES) {
            if let Some(fp) = item.get("file_path").and_then(Value::as_str) {
                let short = shorten_path(fp, 60);
                if item.get("error").is_some() {
                    renderer.line(MessageStyle::ToolError, &format!("  ✗ {short}"))?;
                } else {
                    let lines_info = item
                        .get("ranges")
                        .and_then(Value::as_array)
                        .map(|ranges| {
                            let total_lines: u64 =
                                ranges.iter().filter_map(|r| r.get("lines_read").and_then(Value::as_u64)).sum();
                            format!(" ({total_lines} lines)")
                        })
                        .unwrap_or_default();
                    renderer.line(MessageStyle::ToolDetail, &format!("  ✓ {short}{lines_info}"))?;
                }
            }
        }
        if items.len() > MAX_BATCH_DISPLAY_FILES {
            renderer.line(MessageStyle::ToolDetail, &format!("  … +{} more", items.len() - MAX_BATCH_DISPLAY_FILES))?;
        }
        return Ok(());
    }

    // Single file read: show summary line
    let lines_read = get_u64(val, "lines_read");
    let start_line = get_u64(val, "start_line");
    let end_line = get_u64(val, "end_line");
    let has_more = val.get("has_more").and_then(Value::as_bool).unwrap_or(false);

    let summary = if let Some(n) = lines_read {
        if has_more {
            format!("Read {n} lines (more available)")
        } else {
            format!("Read {n} lines")
        }
    } else if let (Some(start), Some(end)) = (start_line, end_line) {
        let count = end.saturating_sub(start) + 1;
        format!("Read lines {start}-{end} ({count} lines)")
    } else if let Some(content) = get_string(val, "content") {
        let count = content.lines().count();
        format!("Read {count} lines")
    } else {
        return Ok(());
    };
    render_tree_detail(renderer, &summary)?;

    Ok(())
}

const MAX_BATCH_DISPLAY_FILES: usize = 10;

fn shorten_path(path: &str, max_len: usize) -> String {
    if preview::display_width(path) <= max_len {
        return path.to_string();
    }
    if let Some(name) = std::path::Path::new(path).file_name() {
        let name_str = name.to_string_lossy();
        if let Some(parent) = std::path::Path::new(path).parent() {
            let parent_str = parent.to_string_lossy();
            let reserved = preview::display_width(name_str.as_ref()) + 2; // ellipsis + slash
            let budget = max_len.saturating_sub(reserved);
            if budget > 0 && preview::display_width(parent_str.as_ref()) > budget {
                let parent_tail = preview::suffix_for_display_width(parent_str.as_ref(), budget);
                return format!("…{parent_tail}/{name_str}");
            }
        }
        return name_str.to_string();
    }
    preview::truncate_to_display_width(path, max_len).to_string()
}

/// Render diff content lines with proper truncation and styling (compact format)
fn render_diff_content(
    renderer: &mut AnsiRenderer,
    diff_content: &str,
    git_styles: &GitStyles,
    ls_styles: &LsStyles,
) -> Result<()> {
    let plain_diff = strip_ansi_codes(diff_content);
    render_diff_content_block(
        renderer,
        plain_diff.as_ref(),
        Some(tools::WRITE_FILE),
        git_styles,
        ls_styles,
        MessageStyle::ToolDetail,
        ToolOutputMode::Compact,
        usize::MAX,
    )
}

pub(super) fn colourize_diff_summary_line(line: &str, _supports_colour: bool) -> Option<String> {
    let trimmed = line.trim_start();
    let is_summary = trimmed.contains(" file changed")
        || trimmed.contains(" files changed")
        || trimmed.contains(" insertion(+)")
        || trimmed.contains(" insertions(+)")
        || trimmed.contains(" deletion(-)")
        || trimmed.contains(" deletions(-)");
    if is_summary { Some(line.to_string()) } else { None }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
