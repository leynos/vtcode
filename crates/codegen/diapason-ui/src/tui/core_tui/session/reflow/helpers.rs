use anstyle::Effects;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

use crate::tui::config::constants::ui;

use super::super::message::{MessageLine, TranscriptLine};
use crate::tui::core_tui::types::InlineMessageKind;

/// Rule fill pattern for Fieldset-style info/warning/error blocks.
///
/// Mirrors `ratatui_cheese::fieldset::FieldsetFill`, mapping each message kind
/// to a distinct fill: Error → `Slash`, Info → `Dash`, Warning → `Thick`. The
/// Unicode glyphs fall back to ASCII on terminals without Unicode support.
pub(super) fn rule_fill(kind: InlineMessageKind, border_type: ratatui::widgets::BorderType) -> &'static str {
    let unicode = matches!(border_type, ratatui::widgets::BorderType::Rounded);
    match kind {
        // Slash fill (`/`) — already ASCII-safe.
        InlineMessageKind::Error => "/",
        // Thick fill (`━`) with an ASCII fallback.
        InlineMessageKind::Warning => {
            if unicode {
                "━"
            } else {
                "="
            }
        }
        // Dash fill (`─`) with an ASCII fallback.
        _ => {
            if unicode {
                ui::INLINE_BLOCK_HORIZONTAL
            } else {
                "-"
            }
        }
    }
}

/// Check if trimmed, ANSI-stripped text starts with a tool summary prefix.
///
/// Shared by both `is_tool_summary_line` and `reflow_tool_lines` to avoid
/// duplicating the prefix list (DRY).
pub(super) fn has_summary_prefix(text: &str) -> bool {
    let stripped = super::super::text_utils::strip_ansi_codes(text);
    stripped.starts_with("• ")
        || stripped.starts_with("  ├ ")
        || stripped.starts_with("  └ ")
        || stripped.starts_with("  │ ")
}

pub(crate) fn parse_tool_call_prefix(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("• ")?;
    let verb_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    if verb_end == 0 {
        return None;
    }
    let verb = &rest[..verb_end];
    let prefix_len = "• ".len() + verb.len();
    Some((verb, &text[..prefix_len]))
}

pub(super) fn is_tool_summary_line(message: &MessageLine) -> bool {
    let text: String = message.segments.iter().map(|segment| segment.text.as_str()).collect();
    has_summary_prefix(&text)
}

pub(super) fn is_bullet_summary_text(text: &str) -> bool {
    let stripped = super::super::text_utils::strip_ansi_codes(text);
    stripped.starts_with("• ")
}

pub(super) fn is_tree_detail_text(text: &str) -> bool {
    let stripped = super::super::text_utils::strip_ansi_codes(text);
    stripped.starts_with("  └ ") || stripped.starts_with("  ├ ") || stripped.starts_with("  │ ")
}

/// Returns `true` when the next message starts a tool-block boundary that owns
/// its own top spacing (Tool/Pty header or Info tool-summary line).
///
/// Trailing spacing from the previous block must be suppressed in this case so
/// the boundary contributes exactly one gap (the tool top, which is clamped to
/// a minimum of 1). Centralizes the check previously duplicated in
/// `reflow_message_lines` and `reflow_tool_lines`.
pub(super) fn next_is_tool_block(next: Option<&MessageLine>) -> bool {
    match next {
        Some(line) if line.kind == InlineMessageKind::Tool || line.kind == InlineMessageKind::Pty => true,
        Some(line) if line.kind == InlineMessageKind::Info => is_tool_summary_line(line),
        _ => false,
    }
}

/// Returns `true` when a reflowed ratatui line is visually blank (no segments
/// or only whitespace, e.g. `""` or `"  "`). Whitespace-only rows defeat a
/// naive `segments.is_empty()` check and must collapse like empty rows.
pub(super) fn line_is_blank(line: &Line<'_>) -> bool {
    line.spans.iter().all(|span| span.content.trim().is_empty())
}

/// Returns `true` when a transcript line is visually blank. Mirrors
/// [`line_is_blank`] for the [`TranscriptLine`] wrapper.
pub(super) fn transcript_line_is_blank(line: &TranscriptLine) -> bool {
    line_is_blank(&line.line)
}

/// Push `count` blank lines, discounting one when `lines` already ends with a
/// blank row.
///
/// Content may already end with a blank row (e.g. agent text ending in `\n\n`
/// or markdown paragraph gaps). Without the discount the content blank and the
/// requested inter-block gap stack (2 rows for the default spacing of 1).
/// With it the boundary contributes exactly `count` rows, preserving the
/// `message_block_spacing` 0–2 range while keeping the cosy rhythm.
pub(super) fn push_spacing_blanks(lines: &mut Vec<Line<'static>>, count: usize) {
    let mut remaining = count;
    if lines.last().is_some_and(line_is_blank) {
        remaining = remaining.saturating_sub(1);
    }
    lines.extend(std::iter::repeat_with(Line::default).take(remaining));
}

/// [`push_spacing_blanks`] for reflowed transcript lines.
pub(super) fn push_spacing_transcript_lines(lines: &mut Vec<TranscriptLine>, count: usize) {
    let mut remaining = count;
    if lines.last().is_some_and(transcript_line_is_blank) {
        remaining = remaining.saturating_sub(1);
    }
    lines.extend(std::iter::repeat_with(TranscriptLine::default).take(remaining));
}

/// Remove trailing blank transcript rows so the caller can append exactly one
/// inter-block separator. Prevents content trailing blanks (e.g. markdown
/// paragraph gaps, `\n\n` endings) from stacking with message gaps.
pub(super) fn trim_trailing_blank_transcript_lines(lines: &mut Vec<TranscriptLine>) {
    while lines.last().is_some_and(transcript_line_is_blank) {
        lines.pop();
    }
}

pub(super) fn agent_code_continuation_prefix(message: &MessageLine) -> Option<String> {
    let first_segment = message.segments.iter().find(|segment| !segment.text.is_empty())?;
    if !first_segment.style.effects.contains(Effects::DIMMED) {
        return None;
    }

    numbered_code_gutter_prefix(&first_segment.text)
}

fn numbered_code_gutter_prefix(text: &str) -> Option<String> {
    let mut chars = text.char_indices().peekable();
    let mut prefix_end = 0usize;

    while let Some((idx, ch)) = chars.peek().copied() {
        if ch == ' ' {
            prefix_end = idx + ch.len_utf8();
            chars.next();
        } else {
            break;
        }
    }

    let mut saw_digits = false;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            saw_digits = true;
            prefix_end = idx + ch.len_utf8();
            chars.next();
        } else {
            break;
        }
    }
    if !saw_digits {
        return None;
    }

    if let Some((idx, '-')) = chars.peek().copied() {
        prefix_end = idx + 1;
        chars.next();

        let mut saw_range_digits = false;
        while let Some((idx, ch)) = chars.peek().copied() {
            if ch.is_ascii_digit() {
                saw_range_digits = true;
                prefix_end = idx + ch.len_utf8();
                chars.next();
            } else {
                break;
            }
        }
        if !saw_range_digits {
            return None;
        }
    }

    let mut trailing_spaces = 0usize;
    while let Some((idx, ch)) = chars.peek().copied() {
        if ch == ' ' {
            trailing_spaces += 1;
            prefix_end = idx + ch.len_utf8();
            chars.next();
        } else {
            break;
        }
    }
    if trailing_spaces < 2 {
        return None;
    }

    Some(" ".repeat(UnicodeWidthStr::width(&text[..prefix_end])))
}

pub(super) fn split_tool_spans(spans: Vec<Span<'static>>) -> Vec<Vec<Span<'static>>> {
    let mut lines: Vec<Vec<Span<'static>>> = Vec::with_capacity(spans.len());
    let mut current: Vec<Span<'static>> = Vec::with_capacity(spans.len());

    for span in spans {
        let style = span.style;
        let text = span.content.into_owned();
        let mut parts = text.split('\n').peekable();
        while let Some(part) = parts.next() {
            if !part.is_empty() {
                current.push(Span::styled(part.to_string(), style));
            }
            if parts.peek().is_some() {
                lines.push(std::mem::take(&mut current));
            }
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::core_tui::types::{InlineSegment, InlineTextStyle};
    use std::sync::Arc;

    fn message_line(kind: InlineMessageKind, text: &str) -> MessageLine {
        MessageLine {
            kind,
            segments: vec![InlineSegment {
                text: text.to_string(),
                style: Arc::new(InlineTextStyle::default()),
            }],
            link_ranges: Vec::new(),
            revision: 0,
        }
    }

    fn text_line(text: &str) -> TranscriptLine {
        TranscriptLine {
            line: Line::from(text.to_string()),
            explicit_links: Vec::new(),
        }
    }

    #[test]
    fn line_is_blank_covers_empty_and_whitespace_rows() {
        assert!(line_is_blank(&Line::default()));
        assert!(line_is_blank(&Line::from("   ".to_string())));
        assert!(!line_is_blank(&Line::from("answer".to_string())));
    }

    #[test]
    fn push_spacing_discounts_an_existing_trailing_blank() {
        let mut lines = vec![text_line("answer"), TranscriptLine::default()];
        push_spacing_transcript_lines(&mut lines, 1);
        assert_eq!(lines.len(), 2, "existing blank + requested 1 must not stack");

        let mut lines = vec![text_line("answer"), TranscriptLine::default()];
        push_spacing_transcript_lines(&mut lines, 2);
        assert_eq!(lines.len(), 3, "existing blank discounts exactly one row");
    }

    #[test]
    fn push_spacing_blanks_covers_plain_lines() {
        let mut lines = vec![Line::from("tool output".to_string())];
        push_spacing_blanks(&mut lines, 1);
        assert_eq!(lines.len(), 2);

        push_spacing_blanks(&mut lines, 1);
        assert_eq!(lines.len(), 2, "existing blank + requested 1 must not stack");

        push_spacing_blanks(&mut lines, 0);
        assert_eq!(lines.len(), 2, "zero requested rows must not add rows");
    }

    #[test]
    fn trim_trailing_blank_transcript_lines_keeps_head_content() {
        let mut lines = vec![
            text_line("answer"),
            TranscriptLine::default(),
            TranscriptLine::default(),
        ];
        trim_trailing_blank_transcript_lines(&mut lines);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn next_is_tool_block_covers_tool_pty_and_summaries() {
        assert!(next_is_tool_block(Some(&message_line(InlineMessageKind::Tool, "• Ran x"))));
        assert!(next_is_tool_block(Some(&message_line(InlineMessageKind::Pty, "output"))));
        assert!(next_is_tool_block(Some(&message_line(InlineMessageKind::Info, "• Ran x"))));
        assert!(!next_is_tool_block(Some(&message_line(InlineMessageKind::Agent, "answer"))));
        assert!(!next_is_tool_block(Some(&message_line(InlineMessageKind::Info, "plain status"))));
        assert!(!next_is_tool_block(None));
    }
}
