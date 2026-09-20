//! Shared preview formatting helpers.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeadTailPreview<'a, T> {
    pub head: &'a [T],
    pub tail: &'a [T],
    pub hidden_count: usize,
    total: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextLineExcerpt<'a> {
    pub head: Vec<&'a str>,
    pub tail: Vec<&'a str>,
    pub hidden_count: usize,
    pub total: usize,
}

pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

pub fn truncate_to_display_width(text: &str, max_width: usize) -> &str {
    if max_width == 0 {
        return "";
    }
    if display_width(text) <= max_width {
        return text;
    }

    let mut consumed_width = 0usize;
    for (idx, ch) in text.char_indices() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if consumed_width + char_width > max_width {
            return text.get(..idx).unwrap_or_default();
        }
        consumed_width += char_width;
    }

    text
}

pub fn truncate_with_ellipsis(text: &str, max_width: usize, ellipsis: &str) -> String {
    if max_width == 0 {
        return String::new();
    }
    if display_width(text) <= max_width {
        return text.to_string();
    }

    let ellipsis_width = display_width(ellipsis);
    if ellipsis_width >= max_width {
        return truncate_to_display_width(ellipsis, max_width).to_string();
    }

    let truncated = truncate_to_display_width(text, max_width - ellipsis_width);
    format!("{truncated}{ellipsis}")
}

pub fn pad_to_display_width(text: &str, width: usize, pad_char: char) -> String {
    let current = display_width(text);
    if current >= width {
        return text.to_string();
    }

    let padding = pad_char.to_string().repeat(width - current);
    format!("{text}{padding}")
}

pub fn suffix_for_display_width(value: &str, max_width: usize) -> &str {
    if display_width(value) <= max_width {
        return value;
    }
    if max_width == 0 {
        return "";
    }

    let mut consumed_width = 0usize;
    let mut start_idx = value.len();
    for (idx, ch) in value.char_indices().rev() {
        let char_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if consumed_width + char_width > max_width {
            break;
        }
        consumed_width += char_width;
        start_idx = idx;
    }

    value.get(start_idx..).unwrap_or_default()
}

pub fn format_hidden_lines_summary(hidden: usize) -> String {
    if hidden == 1 {
        "… +1 line".to_string()
    } else {
        format!("… +{hidden} lines")
    }
}

fn split_head_tail_preview<'a, T>(items: &'a [T], head: usize, tail: usize) -> HeadTailPreview<'a, T> {
    let total = items.len();
    if total <= head.saturating_add(tail) {
        return HeadTailPreview {
            head: items,
            tail: items.get(total..).unwrap_or_default(),
            hidden_count: 0,
            total,
        };
    }

    let head_count = head.min(total);
    let tail_count = tail.min(total.saturating_sub(head_count));
    let hidden_count = total.saturating_sub(head_count + tail_count);

    HeadTailPreview {
        head: items.get(..head_count).unwrap_or_default(),
        tail: items.get(total - tail_count..).unwrap_or_default(),
        hidden_count,
        total,
    }
}

pub fn split_head_tail_preview_with_limit<'a, T>(
    items: &'a [T],
    limit: usize,
    preferred_head: usize,
) -> HeadTailPreview<'a, T> {
    if limit == 0 {
        return HeadTailPreview {
            head: items.get(..0).unwrap_or_default(),
            tail: items.get(..0).unwrap_or_default(),
            hidden_count: items.len(),
            total: items.len(),
        };
    }

    if items.len() <= limit {
        return HeadTailPreview {
            head: items,
            tail: items.get(items.len()..).unwrap_or_default(),
            hidden_count: 0,
            total: items.len(),
        };
    }

    let (head, tail) = summary_window(limit, preferred_head);
    split_head_tail_preview(items, head, tail)
}

pub fn summary_window(limit: usize, preferred_head: usize) -> (usize, usize) {
    if limit <= 2 {
        return (0, limit);
    }

    let head = preferred_head.min((limit - 1) / 2).max(1);
    let tail = limit.saturating_sub(head + 1).max(1);
    (head, tail)
}

pub fn excerpt_text_lines<'a>(text: &'a str, head: usize, tail: usize) -> TextLineExcerpt<'a> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if total <= head.saturating_add(tail) {
        return TextLineExcerpt {
            head: lines,
            tail: Vec::new(),
            hidden_count: 0,
            total,
        };
    }

    let head_count = head.min(total);
    let tail_count = tail.min(total.saturating_sub(head_count));
    let hidden_count = total.saturating_sub(head_count + tail_count);

    TextLineExcerpt {
        head: lines.get(..head_count).unwrap_or_default().to_vec(),
        tail: lines.get(total - tail_count..).unwrap_or_default().to_vec(),
        hidden_count,
        total,
    }
}

pub fn excerpt_text_lines_with_limit<'a>(text: &'a str, limit: usize, preferred_head: usize) -> TextLineExcerpt<'a> {
    let lines: Vec<&str> = text.lines().collect();
    let preview = split_head_tail_preview_with_limit(lines.as_slice(), limit, preferred_head);

    TextLineExcerpt {
        head: preview.head.to_vec(),
        tail: preview.tail.to_vec(),
        hidden_count: preview.hidden_count,
        total: preview.total,
    }
}

fn format_hidden_bytes_summary(hidden: usize) -> String {
    format!("… [{hidden} bytes omitted] …")
}

pub fn condense_text_bytes(content: &str, head_bytes: usize, tail_bytes: usize) -> String {
    let byte_len = content.len();
    let max_inline = head_bytes + tail_bytes;
    if byte_len <= max_inline {
        return content.to_string();
    }

    let head_end = floor_char_boundary(content, head_bytes);
    let tail_start_raw = byte_len.saturating_sub(tail_bytes);
    let tail_start = ceil_char_boundary(content, tail_start_raw);

    let omitted = byte_len.saturating_sub(head_end).saturating_sub(byte_len - tail_start);

    format!(
        "{}\n\n{}\n\n{}",
        content.get(..head_end).unwrap_or_default(),
        format_hidden_bytes_summary(omitted),
        content.get(tail_start..).unwrap_or_default()
    )
}

pub fn tail_preview_text(content: &str, tail_bytes: usize, max_lines: usize) -> String {
    if content.is_empty() {
        return String::new();
    }

    let tail_start = ceil_char_boundary(content, content.len().saturating_sub(tail_bytes));
    let tail_slice = content.get(tail_start..).unwrap_or_default();

    let line_start = if max_lines == 0 {
        0
    } else {
        tail_slice
            .as_bytes()
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, byte)| **byte == b'\n')
            .nth(max_lines - 1)
            .map_or(0, |(index, _)| index.saturating_add(1))
    };

    let preview = tail_slice.get(line_start..).unwrap_or_default();
    let omitted = tail_start.saturating_add(line_start);
    if omitted == 0 {
        return preview.to_string();
    }

    format!("{}\n{}", format_hidden_bytes_summary(omitted), preview)
}

fn floor_char_boundary(value: &str, index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }

    let mut i = index;
    while i > 0 && !value.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(value: &str, index: usize) -> usize {
    if index >= value.len() {
        return value.len();
    }

    let mut i = index;
    while i < value.len() && !value.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
#[path = "preview_tests.rs"]
mod tests;
