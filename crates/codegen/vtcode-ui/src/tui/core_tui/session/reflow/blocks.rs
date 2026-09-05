use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;
use vtcode_commons::diff_paths::{is_diff_addition_line, is_diff_deletion_line};

use super::super::super::style::{
    ratatui_colour_from_ansi, ratatui_pty_detail_style_from_inline, ratatui_pty_style_from_inline,
    ratatui_style_from_ansi, ratatui_style_from_inline,
};
use super::super::super::types::{InlineLinkRange, InlineMessageKind, InlineTextStyle};
use super::super::message::RenderedTranscriptLink;
use super::super::styling::tool_inline_style_for;
use super::super::{Session, TranscriptLine, render, text_utils};
use super::helpers::{
    has_summary_prefix, is_bullet_summary_text, is_tool_summary_line, is_tree_detail_text, parse_tool_call_prefix,
    push_spacing_blanks, push_spacing_transcript_lines, split_tool_spans,
};
use crate::tui::config::constants::ui;

impl Session {
    fn opaque_tool_header_text_style(&self, style: Style) -> Style {
        let mut style = style.remove_modifier(Modifier::DIM);
        let is_subdued_foreground = [self.theme.pty_body, self.theme.tool_body]
            .into_iter()
            .flatten()
            .map(ratatui_colour_from_ansi)
            .any(|colour| style.fg == Some(colour));
        if is_subdued_foreground && let Some(foreground) = self.theme.foreground.map(ratatui_colour_from_ansi) {
            style = style.fg(foreground);
        }
        style
    }

    fn tool_header_action_style(&self, action: &str) -> Style {
        if action == "Ran" {
            let style = InlineTextStyle {
                colour: self.theme.primary.or(self.theme.foreground),
                ..InlineTextStyle::default()
            }
            .bold();
            return ratatui_style_from_inline(&style, self.theme.foreground).remove_modifier(Modifier::DIM);
        }

        let tool_style = tool_inline_style_for(action, &self.theme);
        let fallback = self.theme.tool_accent.or(self.theme.primary).or(self.theme.foreground);
        ratatui_style_from_ansi(tool_style.to_ansi_style(fallback)).remove_modifier(Modifier::DIM)
    }

    fn tool_header_body_style(&self) -> Style {
        let style = InlineTextStyle {
            // `tool_body` is an accent colour in several themes. It must not be
            // used as the fallback here because a whole status-coloured header
            // would still appear coloured after the status span is split off.
            colour: self.theme.foreground,
            ..InlineTextStyle::default()
        };
        ratatui_style_from_inline(&style, self.theme.foreground).remove_modifier(Modifier::DIM)
    }

    fn wrapped_diff_continuation_prefix(line_text: &str) -> Option<String> {
        let trimmed = line_text.trim_start();
        if is_diff_deletion_line(trimmed) || is_diff_addition_line(trimmed) {
            let marker_pos = line_text.find(['-', '+'])?;
            let marker_end = marker_pos + 1;
            let after = line_text.get(marker_end..)?;
            let extra_space = after.chars().take_while(|c| *c == ' ').count();
            let end = marker_end + extra_space;
            return line_text.get(..end).map(ToOwned::to_owned);
        }

        // Numbered diff line: "<line_no><spaces><+|-><spaces><code>"
        let mut idx = 0usize;
        for ch in line_text.chars() {
            if ch == ' ' {
                idx += ch.len_utf8();
            } else {
                break;
            }
        }

        let rest = line_text.get(idx..)?;
        let digits_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits_len == 0 {
            return None;
        }
        let mut offset = idx + rest.chars().take(digits_len).map(char::len_utf8).sum::<usize>();
        let after_digits = line_text.get(offset..)?;
        let space_after_digits = after_digits.chars().take_while(|c| *c == ' ').count();
        if space_after_digits == 0 {
            return None;
        }
        offset += after_digits.chars().take(space_after_digits).map(char::len_utf8).sum::<usize>();

        let marker = line_text.get(offset..)?.chars().next()?;
        if !matches!(marker, '+' | '-') {
            return None;
        }
        offset += marker.len_utf8();

        let after_marker = line_text.get(offset..)?;
        let space_after_marker = after_marker.chars().take_while(|c| *c == ' ').count();
        if space_after_marker == 0 {
            return None;
        }
        offset += after_marker.chars().take(space_after_marker).map(char::len_utf8).sum::<usize>();

        let prefix_width = UnicodeWidthStr::width(line_text.get(..offset)?);
        Some(" ".repeat(prefix_width))
    }

    /// Wrap content with left and right borders
    fn wrap_block_lines(
        &self,
        first_prefix: &str,
        continuation_prefix: &str,
        content: Vec<Span<'static>>,
        max_width: usize,
        border_style: Style,
    ) -> Vec<Line<'static>> {
        self.wrap_block_lines_with_options(first_prefix, continuation_prefix, content, max_width, border_style, true)
    }

    /// Wrap content with left border only (no right border)
    fn wrap_block_lines_no_right_border(
        &self,
        first_prefix: &str,
        continuation_prefix: &str,
        content: Vec<Span<'static>>,
        max_width: usize,
        border_style: Style,
    ) -> Vec<Line<'static>> {
        self.wrap_block_lines_with_options(first_prefix, continuation_prefix, content, max_width, border_style, false)
    }

    /// Wrap content with configurable border options
    fn wrap_block_lines_with_options(
        &self,
        first_prefix: &str,
        continuation_prefix: &str,
        content: Vec<Span<'static>>,
        max_width: usize,
        border_style: Style,
        show_right_border: bool,
    ) -> Vec<Line<'static>> {
        if max_width < 2 {
            let fallback = if show_right_border {
                format!("{first_prefix}││")
            } else {
                format!("{first_prefix}│")
            };
            return vec![Line::from(fallback).style(border_style)];
        }

        let right_border = if show_right_border {
            ui::INLINE_BLOCK_BODY_RIGHT
        } else {
            ""
        };
        let first_prefix_width = UnicodeWidthStr::width(first_prefix);
        let continuation_prefix_width = UnicodeWidthStr::width(continuation_prefix);
        let prefix_width = first_prefix_width.max(continuation_prefix_width);
        let border_width = UnicodeWidthStr::width(right_border);
        let consumed_width = prefix_width.saturating_add(border_width);
        let content_width = max_width.saturating_sub(consumed_width);

        if max_width == usize::MAX {
            let mut spans = vec![Span::styled(first_prefix.to_owned(), border_style)];
            spans.extend(content);
            if show_right_border {
                spans.push(Span::styled(right_border.to_owned(), border_style));
            }
            return vec![Line::from(spans)];
        }

        let diff_continuation_prefix = content.first().and_then(|span| {
            let text: &str = span.content.as_ref();
            Self::wrapped_diff_continuation_prefix(text)
        });

        let line_text = content.iter().map(|span| span.content.as_ref()).collect::<String>();
        let tree_continuation_prefix = text_utils::compact_tree_continuation_prefix(&line_text);
        let content_line = Line::from(content);
        // URL-aware wrapping historically reused the first span's style for
        // every fragment. That makes a path-bearing command header inherit
        // the status-coloured bullet. Preserve header spans through the normal
        // style-aware wrapper; URL-aware wrapping remains useful for output.
        let mut wrapped = if let Some(prefix) = tree_continuation_prefix {
            text_utils::wrap_line_with_hanging_prefix(content_line, content_width, &prefix)
        } else if line_is_tool_command_header(&content_line) {
            text_utils::wrap_line(content_line, content_width)
        } else {
            self.wrap_line(content_line, content_width)
        };
        if wrapped.is_empty() {
            wrapped.push(Line::default());
        }

        // Add borders to each wrapped line
        for (idx, line) in wrapped.iter_mut().enumerate() {
            let line_width = line.spans.iter().map(|s| s.width()).sum::<usize>();
            let padding = if show_right_border {
                content_width.saturating_sub(line_width)
            } else {
                0
            };

            let active_prefix = if idx == 0 { first_prefix } else { continuation_prefix };
            let mut new_spans = vec![Span::styled(active_prefix.to_owned(), border_style)];

            // For diff lines, preserve hanging indent/prefix on continuation lines.
            if idx > 0
                && let Some(ref prefix) = diff_continuation_prefix
            {
                // Add the diff prefix with dimmed style to match diff appearance
                let prefix_style = border_style.add_modifier(Modifier::DIM);
                new_spans.push(Span::styled(prefix.clone(), prefix_style));
            }

            new_spans.append(&mut line.spans);
            if padding > 0 {
                new_spans.push(Span::styled(" ".repeat(padding), Style::default()));
            }
            if show_right_border {
                new_spans.push(Span::styled(right_border.to_owned(), border_style));
            }
            line.spans = new_spans;
        }

        wrapped
    }

    /// Reflow tool output lines with appropriate formatting
    ///
    /// Tool blocks are visually grouped with:
    /// - Consistent indentation (2 spaces)
    /// - Dimmed styling for less visual weight
    /// - Optional spacing after tool block ends
    pub(super) fn reflow_tool_lines(&self, index: usize, width: u16) -> Vec<Line<'static>> {
        let Some(line) = self.lines.get(index) else {
            return vec![Line::default()];
        };

        let max_width = if width == 0 { usize::MAX } else { width as usize };

        let border_style = self.styles.border_style();

        // Check if this is the start of a tool block
        let prev_is_tool = if index > 0 {
            self.lines
                .get(index - 1)
                .map(|prev| prev.kind == InlineMessageKind::Tool)
                .unwrap_or(false)
        } else {
            false
        };
        let is_start = !prev_is_tool;

        let next_is_tool = self.lines.get(index + 1).is_some_and(|next| {
            if next.kind == InlineMessageKind::Tool {
                return true;
            }
            // A Tool header followed by its Info tree details (e.g. "• Search code"
            // -> "  └ File types:") should be treated as a single visual block
            // without an extra blank line between header and details.
            if next.kind == InlineMessageKind::Info {
                let text: String = next.segments.iter().map(|s| s.text.as_str()).collect();
                if is_tree_detail_text(&text) {
                    return true;
                }
            }
            false
        });
        let is_end = !next_is_tool;

        let mut lines = Vec::new();

        // Visual separator at the start of a tool block. Single ownership: the
        // previous block suppresses its trailing gap before tool blocks (see
        // `next_is_tool_block`), so this top gap is the only one.
        if is_start && self.should_add_tool_block_top_spacing(index) {
            push_spacing_blanks(&mut lines, self.tool_block_spacing());
        }

        let content = render::render_tool_segments(self, line);
        let split_lines = split_tool_spans(content);
        let summary_prefix = ui::INLINE_TOOL_HEADER_GUTTER;
        let summary_continuation = ui::INLINE_TOOL_HEADER_CONTINUATION;
        let detail_prefix = ui::INLINE_TOOL_DETAIL_GUTTER;
        let detail_continuation = ui::INLINE_TOOL_DETAIL_CONTINUATION;
        let detail_border_style = border_style.add_modifier(Modifier::DIM);

        for line_spans in split_lines {
            let line_text: String = line_spans.iter().map(|span| span.content.as_ref()).collect();
            let is_summary = has_summary_prefix(&line_text);
            if is_summary {
                // Style the tool call prefix ("• <Action>") with tool-specific ANSI colour + bold.
                let mut styled_spans = Vec::with_capacity(line_spans.len() + 1);
                for (i, span) in line_spans.into_iter().enumerate() {
                    if i == 0 {
                        let text = span.content.clone().into_owned();
                        let style = span.style;
                        if let Some((action, prefix)) = parse_tool_call_prefix(&text) {
                            let mut bullet_style = style.remove_modifier(Modifier::DIM);
                            if bullet_style.fg.is_none() {
                                if let Some(c) = self.theme.foreground.map(ratatui_colour_from_ansi) {
                                    bullet_style = bullet_style.fg(c);
                                }
                            }
                            if bullet_style.bg.is_none() {
                                if let Some(c) = self.theme.background.map(ratatui_colour_from_ansi) {
                                    bullet_style = bullet_style.bg(c);
                                }
                            }
                            let action_start = prefix.len() - action.len();
                            styled_spans.push(Span::styled(prefix[..action_start].to_owned(), bullet_style));
                            styled_spans.push(Span::styled(action.to_owned(), self.tool_header_action_style(action)));
                            let rest = &text[prefix.len()..];
                            if !rest.is_empty() {
                                styled_spans.push(Span::styled(rest.to_owned(), self.tool_header_body_style()));
                            }
                        } else {
                            styled_spans.push(Span::styled(text, style.remove_modifier(Modifier::DIM)));
                        }
                    } else {
                        styled_spans.push(Span::styled(
                            span.content.into_owned(),
                            self.opaque_tool_header_text_style(span.style),
                        ));
                    }
                }
                lines.extend(self.wrap_block_lines(
                    summary_prefix,
                    summary_continuation,
                    styled_spans,
                    max_width,
                    border_style,
                ));
            } else {
                // Dim tool output and avoid right-side padding borders.
                // Detail rows are nested under their header with an extra indent,
                // and wrapped lines keep the tree marker aligned via hanging indent.
                let mut detail_spans = line_spans;
                for span in &mut detail_spans {
                    span.style = span.style.add_modifier(Modifier::DIM);
                }
                lines.extend(self.wrap_block_lines_no_right_border(
                    detail_prefix,
                    detail_continuation,
                    detail_spans,
                    max_width,
                    detail_border_style,
                ));
            }
        }

        // Spacing after a tool block for clean separation (single ownership:
        // the next tool-block start suppresses its own top gap inside chains).
        if is_end {
            push_spacing_blanks(&mut lines, self.tool_block_spacing());
        }

        if lines.is_empty() {
            lines.push(Line::default());
        }

        lines
    }

    /// Check if a PTY block has actual content
    fn pty_block_has_content(&self, index: usize) -> bool {
        if self.lines.is_empty() {
            return false;
        }

        let mut start = index;
        while start > 0 {
            let Some(previous) = self.lines.get(start - 1) else {
                break;
            };
            if previous.kind != InlineMessageKind::Pty {
                break;
            }
            start -= 1;
        }

        let mut end = index;
        while end + 1 < self.lines.len() {
            let Some(next) = self.lines.get(end + 1) else {
                break;
            };
            if next.kind != InlineMessageKind::Pty {
                break;
            }
            end += 1;
        }

        if start > end || end >= self.lines.len() {
            tracing::warn!("invalid range: start={}, end={}, len={}", start, end, self.lines.len());
            return false;
        }

        for line in &self.lines[start..=end] {
            if line.segments.iter().any(|segment| !segment.text.trim().is_empty()) {
                return true;
            }
        }

        false
    }

    /// Reflow PTY output lines with appropriate borders and formatting
    pub(crate) fn reflow_pty_lines(&self, index: usize, width: u16) -> Vec<TranscriptLine> {
        let Some(line) = self.lines.get(index) else {
            return vec![TranscriptLine::default()];
        };

        let max_width = if width == 0 { usize::MAX } else { width as usize };

        if !self.pty_block_has_content(index) {
            return Vec::new();
        }

        let border_style = self.styles.border_style();

        let prev_is_pty = index
            .checked_sub(1)
            .and_then(|prev| self.lines.get(prev))
            .map(|prev| prev.kind == InlineMessageKind::Pty)
            .unwrap_or(false);

        let is_start = !prev_is_pty;
        let is_end = !self
            .lines
            .get(index + 1)
            .is_some_and(|next| next.kind == InlineMessageKind::Pty);

        let mut lines = Vec::with_capacity(line.segments.len());

        let mut combined = String::with_capacity(line.segments.iter().map(|s| s.text.len()).sum());
        for segment in &line.segments {
            combined.push_str(segment.text.as_str());
        }
        if is_start && combined.trim().is_empty() {
            return Vec::new();
        }

        // Render body content - strip ANSI codes to ensure plain text output.
        // Use the session PTY fallback chain (pty_body -> tool_body -> foreground)
        // and apply a consistent dimmed style for terminal output.
        let pty_fallback = self.text_fallback(InlineMessageKind::Pty).or(self.theme.foreground);

        // Command header lines ("• Ran ...", "• Read ...", "• Write ...", etc.)
        // use full-brightness tool-coloured styling so the tool name and arguments
        // are visually distinct from dimmed PTY body text.
        // Every PTY line can begin a new tool command. `is_start` only marks
        // the beginning of the surrounding PTY block, so using it here would
        // incorrectly dim commands that follow an earlier command's output.
        let is_command_header = parse_tool_call_prefix(&combined).is_some();

        let mut body_spans = Vec::with_capacity(line.segments.len() + 1);
        for (i, segment) in line.segments.iter().enumerate() {
            let stripped_text = render::strip_ansi_codes(&segment.text);

            if is_command_header && i == 0 {
                if let Some((action, prefix)) = parse_tool_call_prefix(&stripped_text) {
                    let fg = self.theme.foreground.map(ratatui_colour_from_ansi);
                    let bg = self.theme.background.map(ratatui_colour_from_ansi);

                    let mut bullet_style =
                        ratatui_style_from_inline(&segment.style, pty_fallback).remove_modifier(Modifier::DIM);
                    if bullet_style.fg.is_none() {
                        if let Some(c) = fg {
                            bullet_style = bullet_style.fg(c);
                        }
                    }
                    if bullet_style.bg.is_none() {
                        if let Some(c) = bg {
                            bullet_style = bullet_style.bg(c);
                        }
                    }
                    let bullet = &prefix[..prefix.len() - action.len()];
                    body_spans.push(Span::styled(bullet.to_owned(), bullet_style));

                    body_spans.push(Span::styled(action.to_owned(), self.tool_header_action_style(action)));

                    let rest = &stripped_text[prefix.len()..];
                    if !rest.is_empty() {
                        // A streamed header can arrive as one status-coloured
                        // segment. Keep that status colour on the bullet only;
                        // never let it leak into the command and arguments.
                        body_spans.push(Span::styled(rest.to_owned(), self.tool_header_body_style()));
                    }
                    continue;
                }
            }

            if is_command_header && i == 0 && stripped_text == "• " {
                body_spans.push(Span::styled(
                    stripped_text.into_owned(),
                    ratatui_style_from_inline(&segment.style, pty_fallback).remove_modifier(Modifier::DIM),
                ));
                continue;
            }
            if is_command_header && i == 1 && stripped_text == "Ran" {
                body_spans.push(Span::styled(stripped_text.into_owned(), self.tool_header_action_style("Ran")));
                continue;
            }

            let style = if is_command_header {
                self.opaque_tool_header_text_style(ratatui_style_from_inline(&segment.style, pty_fallback))
            } else {
                ratatui_pty_detail_style_from_inline(&segment.style, pty_fallback, self.theme.background)
            };
            body_spans.push(Span::styled(stripped_text.into_owned(), style));
        }

        let body_prefix = ui::INLINE_PTY_BODY_GUTTER;
        let continuation_prefix = text_utils::pty_wrapped_continuation_prefix(body_prefix, combined.as_str());
        lines.extend(self.wrap_block_lines_no_right_border(
            body_prefix,
            continuation_prefix.as_str(),
            body_spans,
            max_width,
            border_style,
        ));

        if lines.is_empty() {
            lines.push(Line::default());
        }

        if is_end {
            push_spacing_blanks(&mut lines, self.tool_block_spacing());
        }

        let mut transcript_lines =
            build_pty_transcript_lines(lines, &line.link_ranges, body_prefix, continuation_prefix.as_str());
        // Single ownership, as for tool blocks above: the previous block
        // suppresses its trailing gap before PTY blocks, so this top gap is
        // the only one.
        if is_start && self.should_add_tool_block_top_spacing(index) {
            let spacing = self.tool_block_spacing();
            let mut spaced = Vec::with_capacity(transcript_lines.len() + spacing);
            push_spacing_transcript_lines(&mut spaced, spacing);
            spaced.append(&mut transcript_lines);
            transcript_lines = spaced;
        }
        transcript_lines
    }
}

fn build_pty_transcript_lines(
    lines: Vec<Line<'static>>,
    link_ranges: &[InlineLinkRange],
    first_prefix: &str,
    continuation_prefix: &str,
) -> Vec<TranscriptLine> {
    let mut combined_offset = 0usize;
    let mut transcript_lines = Vec::with_capacity(lines.len());

    for (index, line) in lines.into_iter().enumerate() {
        let prefix = if index == 0 { first_prefix } else { continuation_prefix };
        let full_text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
        let body_text = full_text.strip_prefix(prefix).unwrap_or(full_text.as_str());
        let body_end = combined_offset + body_text.len();
        let mut explicit_links = Vec::new();

        for link in link_ranges {
            let start = link.start.max(combined_offset);
            let end = link.end.min(body_end);
            if start >= end {
                continue;
            }

            let local_start = start - combined_offset;
            let local_end = end - combined_offset;
            let start_col = UnicodeWidthStr::width(prefix) + UnicodeWidthStr::width(&body_text[..local_start]);
            let width = UnicodeWidthStr::width(&body_text[local_start..local_end]);
            if width == 0 {
                continue;
            }

            explicit_links.push(RenderedTranscriptLink {
                start: prefix.len() + local_start,
                end: prefix.len() + local_end,
                start_col,
                width,
                target: link.target.clone(),
            });
        }

        transcript_lines.push(TranscriptLine { line, explicit_links });
        combined_offset = body_end;
    }

    transcript_lines
}

fn line_is_tool_command_header(line: &Line<'_>) -> bool {
    let text: String = line.spans.iter().map(|span| span.content.as_ref()).collect();
    let stripped = text_utils::strip_ansi_codes(&text);
    parse_tool_call_prefix(stripped.trim_start()).is_some()
}
