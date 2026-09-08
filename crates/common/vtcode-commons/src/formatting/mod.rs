#![expect(
    clippy::string_slice,
    unused_results,
    reason = "Formatting uses ASCII delimiters and intentionally ignores infallible String mutation results."
)]

//! Unified formatting utilities for UI and logging.

/// Formats a file size using human-readable binary units.
pub fn format_size(size: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if size >= GB {
        format!("{:.1}GB", size as f64 / GB as f64)
    } else if size >= MB {
        format!("{:.1}MB", size as f64 / MB as f64)
    } else if size >= KB {
        format!("{:.1}KB", size as f64 / KB as f64)
    } else {
        format!("{size}B")
    }
}

/// Indent a block of text with the given prefix
pub fn indent_block(text: &str, indent: &str) -> String {
    if indent.is_empty() || text.is_empty() {
        return text.to_string();
    }
    let mut indented = String::with_capacity(text.len() + indent.len() * text.lines().count());
    for (idx, line) in text.split('\n').enumerate() {
        if idx > 0 {
            indented.push('\n');
        }
        if !line.is_empty() {
            indented.push_str(indent);
        }
        indented.push_str(line);
    }
    indented
}

/// Truncate text to a maximum length (in chars) with an optional ellipsis.
pub fn truncate_text(text: &str, max_len: usize, ellipsis: &str) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }

    let mut truncated = text.chars().take(max_len).collect::<String>();
    truncated.push_str(ellipsis);
    truncated
}

/// Truncate text to `max_len` chars, reserving room for `ellipsis` so the
/// returned string never exceeds `max_len` chars.
///
/// This differs from [`truncate_text`], which appends the ellipsis *after*
/// taking `max_len` chars (yielding up to `max_len + ellipsis.len()` chars).
/// Use this when the total rendered width must stay within a hard budget.
///
/// ```
/// # use vtcode_commons::formatting::truncate_within;
/// assert_eq!(truncate_within("hello world", 8, "..."), "hello...");
/// assert_eq!(truncate_within("hi", 8, "..."), "hi");
/// assert_eq!(truncate_within("hello", 3, "…"), "he…");
/// ```
pub fn truncate_within(text: &str, max_len: usize, ellipsis: &str) -> String {
    if text.chars().count() <= max_len {
        return text.to_string();
    }
    let keep = max_len.saturating_sub(ellipsis.chars().count());
    let mut truncated = text.chars().take(keep).collect::<String>();
    truncated.push_str(ellipsis);
    truncated
}

/// Truncate `text` to at most `max_len` chars, keeping a head and a tail joined by
/// a single `…` so context from both ends is preserved.
///
/// Control characters are replaced with spaces before truncation so the result is
/// safe to render in a terminal/TUI. When the text already fits it is returned
/// unchanged (after sanitization).
///
/// This is the canonical middle-truncation helper, shared so the same logic is not
/// re-implemented per crate.
pub fn truncate_middle(text: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let sanitized: String = text
        .chars()
        .map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c })
        .collect();
    let char_count = sanitized.chars().count();
    if char_count <= max_len {
        return sanitized;
    }
    if max_len <= 1 {
        return "…".to_string();
    }
    let head_len = max_len / 2;
    let tail_len = max_len.saturating_sub(head_len + 1);

    let head: String = sanitized.chars().take(head_len).collect();
    let mut result = String::with_capacity(head.len() + tail_len + 1);
    result.push_str(&head);
    result.push('…');
    if tail_len > 0 {
        let mut tail_rev: Vec<char> = sanitized.chars().rev().take(tail_len).collect();
        tail_rev.reverse();
        let tail: String = tail_rev.into_iter().collect();
        result.push_str(&tail);
    }
    result
}

/// Truncate a file path in the middle, preferring to break at path separators.
///
/// Keeps a head and a tail joined by `…`, choosing break points at `/` so the most
/// recognizable parts of the path (directories / file name) are preserved. This is
/// the path-aware sibling of [`truncate_middle`], shared so the same display logic
/// is not re-implemented per crate.
pub fn truncate_path_middle(path: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    let char_count = path.chars().count();
    if char_count <= max_len {
        return path.to_string();
    }
    if max_len <= 1 {
        return "…".to_string();
    }

    // Try to find a good break point at a path separator
    let head_budget = max_len / 2;
    let tail_budget = max_len.saturating_sub(head_budget + 1);

    // Find the last '/' in the head portion
    // Collect chars directly into a String — `String: FromIterator<char>`,
    // so the intermediate `Vec<char>` of the prior two-step collect is redundant.
    let head_str: String = path.chars().take(head_budget).collect();
    let head_break = head_str.rfind('/').unwrap_or(head_budget);

    // Find the first '/' in the tail portion (from the end)
    let tail_chars: Vec<char> = path.chars().rev().take(tail_budget).collect();
    let tail_str: String = tail_chars.iter().rev().collect();
    let tail_break_from_end = tail_str.find('/').map(|pos| tail_str.len() - pos).unwrap_or(tail_budget);

    let head: String = path.chars().take(head_break).collect();
    let tail: String = path
        .chars()
        .rev()
        .take(tail_break_from_end)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    format!("{head}…{tail}")
}

/// Truncate `value` to `max_chars` chars by keeping a head and a tail joined by
/// `marker`, preserving context from both ends of the text.
///
/// Returns `(text, was_truncated)`. When the budget is too small to fit the
/// marker plus meaningful context, falls back to a head-only prefix with a
/// ` [truncated]` suffix, respecting the `max_chars` budget.
///
/// ```
/// # use vtcode_commons::formatting::head_tail_truncate;
/// let (out, truncated) = head_tail_truncate("short", 64, " ... ");
/// assert_eq!(out, "short");
/// assert!(!truncated);
/// ```
pub fn head_tail_truncate(value: &str, max_chars: usize, marker: &str) -> (String, bool) {
    const SUFFIX: &str = " [truncated]";

    let total_chars = value.chars().count();
    if total_chars <= max_chars {
        return (value.to_string(), false);
    }

    let marker_chars = marker.chars().count();
    if max_chars <= marker_chars + 16 {
        let suffix_len = SUFFIX.chars().count();
        let truncated = if max_chars > suffix_len {
            let available = max_chars - suffix_len;
            let mut result = value.chars().take(available).collect::<String>();
            result.push_str(SUFFIX);
            result
        } else {
            value.chars().take(max_chars).collect::<String>()
        };
        return (truncated, true);
    }

    let available = max_chars.saturating_sub(marker_chars);
    let head_chars = (available * 2) / 3;
    let tail_chars = available.saturating_sub(head_chars);
    let head = value.chars().take(head_chars).collect::<String>();
    let tail = value.chars().skip(total_chars.saturating_sub(tail_chars)).collect::<String>();
    let mut truncated = String::with_capacity(max_chars + 20);
    truncated.push_str(&head);
    truncated.push_str(marker);
    truncated.push_str(&tail);
    (truncated, true)
}

/// Word-wrap `text` into lines, allowing `first_width` chars on the first line
/// and `continuation_width` chars on subsequent lines. Wrapping prefers
/// whitespace boundaries and is UTF-8 safe (widths count chars, not bytes).
///
/// Returns an empty vec for blank input. Words longer than the width are split
/// at the width boundary rather than overflowing.
///
/// ```
/// # use vtcode_commons::formatting::wrap_text_words;
/// let lines = wrap_text_words("the quick brown fox", 9, 9);
/// assert_eq!(lines, vec!["the quick", "brown fox"]);
/// assert!(wrap_text_words("   ", 5, 5).is_empty());
/// ```
pub fn wrap_text_words(text: &str, first_width: usize, continuation_width: usize) -> Vec<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut result = Vec::new();
    let mut remaining = trimmed;
    let mut width = first_width.max(1);

    while remaining.chars().take(width + 1).count() > width {
        let split = split_at_word_boundary(remaining, width);
        let (head, tail) = remaining.split_at(split);
        let head = head.trim();
        if head.is_empty() {
            break;
        }
        result.push(head.to_string());
        remaining = tail.trim_start();
        if remaining.is_empty() {
            break;
        }
        width = continuation_width.max(1);
    }

    if !remaining.is_empty() {
        result.push(remaining.to_string());
    }
    result
}

fn split_at_word_boundary(input: &str, width: usize) -> usize {
    let mut last_space: Option<usize> = None;
    for (seen, (idx, ch)) in input.char_indices().enumerate() {
        if seen > width {
            break;
        }
        if ch.is_whitespace() {
            last_space = Some(idx);
        }
    }
    match last_space {
        Some(pos) => pos,
        None => byte_index_for_char_count(input, width),
    }
}

fn byte_index_for_char_count(input: &str, chars: usize) -> usize {
    if chars == 0 {
        return 0;
    }
    let mut seen = 0usize;
    for (idx, ch) in input.char_indices() {
        seen += 1;
        if seen == chars {
            return idx + ch.len_utf8();
        }
    }
    input.len()
}

/// Truncate a string so that the retained prefix is at most `max_bytes` bytes,
/// rounded down to the nearest UTF-8 char boundary.  Returns the truncated
/// prefix with `suffix` appended, or the original string when it already fits.
pub fn truncate_byte_budget(text: &str, max_bytes: usize, suffix: &str) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let mut end = max_bytes.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &text[..end])
}

/// Collapse consecutive whitespace into single spaces, trimming leading/trailing.
///
/// ```
/// # use vtcode_commons::formatting::collapse_whitespace;
/// assert_eq!(collapse_whitespace("  hello   world  "), "hello world");
/// assert_eq!(collapse_whitespace(""), "");
/// ```
#[inline]
pub fn collapse_whitespace(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut pending_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            pending_space = true;
        } else {
            if pending_space && !result.is_empty() {
                result.push(' ');
            }
            result.push(ch);
            pending_space = false;
        }
    }
    result
}

/// Clean reasoning text by trimming trailing whitespace on each line and
/// removing blank lines.
///
/// ```
/// # use vtcode_commons::formatting::clean_reasoning_text;
/// assert_eq!(clean_reasoning_text("line1\n\n\nline2\n"), "line1\nline2");
/// assert_eq!(clean_reasoning_text(""), "");
/// ```
pub fn clean_reasoning_text(text: &str) -> String {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Compact reasoning text for on-screen display.
///
/// Unlike [`clean_reasoning_text`], which removes *all* blank lines, this
/// collapses runs of two or more blank/whitespace-only lines into a single
/// blank line so paragraph structure is preserved while "blank-line spam"
/// from the model is removed. Leading/trailing whitespace on every line is
/// trimmed and leading/trailing blank lines of the whole block are dropped.
///
/// ```
/// # use vtcode_commons::formatting::compact_reasoning_text;
/// assert_eq!(compact_reasoning_text("line1\n\n\n\nline2\n"), "line1\n\nline2");
/// assert_eq!(compact_reasoning_text("  a  \n\n\n  b  \n"), "a\n\nb");
/// assert_eq!(compact_reasoning_text("\n\n\n"), "");
/// assert_eq!(compact_reasoning_text(""), "");
/// ```
pub fn compact_reasoning_text(text: &str) -> String {
    let mut out: Vec<&str> = Vec::with_capacity(text.lines().count());
    let mut prev_blank = false;
    for line in text.lines() {
        let trimmed = line.trim();
        let is_blank = trimmed.is_empty();
        if is_blank {
            if prev_blank {
                continue;
            }
            out.push("");
            prev_blank = true;
        } else {
            out.push(trimmed);
            prev_blank = false;
        }
    }
    while out.first().is_some_and(|l| l.trim().is_empty()) {
        out.remove(0);
    }
    while out.last().is_some_and(|l| l.trim().is_empty()) {
        out.pop();
    }
    out.join("\n")
}

#[cfg(test)]
#[path = "formatting_tests.rs"]
mod tests;
