use super::{Session, message::TranscriptLine};
use diapason_commons::preview;
use ratatui::prelude::*;
use unicode_width::UnicodeWidthStr;

pub(crate) struct QueueOverlay {
    width: u16,
    version: u64,
    lines: Vec<Line<'static>>,
}

impl Session {
    pub(crate) fn set_queued_inputs_entries(&mut self, entries: Vec<String>) {
        self.queued_inputs = entries;
        self.invalidate_queue_overlay();
    }

    pub(crate) fn push_queued_input(&mut self, entry: String) {
        self.queued_inputs.push(entry);
        self.invalidate_queue_overlay();
    }

    pub(crate) fn pop_latest_queued_input(&mut self) -> Option<String> {
        let result = self.queued_inputs.pop();
        if result.is_some() {
            self.invalidate_queue_overlay();
        }
        result
    }

    fn queue_input_lines(&self, width: u16) -> Vec<Line<'static>> {
        if width == 0 || self.queued_inputs.is_empty() {
            return Vec::new();
        }

        const VISIBLE_ITEMS: usize = 5;

        let max_width = width as usize;
        let mut lines = Vec::new();
        let mut prefix_style = self.styles.accent_style();
        prefix_style = prefix_style.add_modifier(Modifier::BOLD);
        let message_style = self.styles.default_style();

        let prefix = "↳ ";
        let prefix_width = UnicodeWidthStr::width(prefix);
        let available = max_width.saturating_sub(prefix_width);

        // The queue is strict FIFO (first queued = first dispatched), so the
        // overlay renders in insertion order: oldest on top, newest directly
        // above the input field. The oldest will be sent first. Multi-line
        // items must be flattened so a raw newline/carriage-return cannot
        // break the layout.
        for entry in self.queued_inputs.iter().take(VISIBLE_ITEMS) {
            let flattened = entry.replace(['\r', '\n'], " ⏎ ");
            let trimmed = truncate_to_width(&flattened, available);
            let mut spans = Vec::with_capacity(2);
            // Skip the prefix when it does not fit (tiny terminal width).
            if available > 0 {
                spans.push(Span::styled(prefix.to_owned(), prefix_style));
            }
            spans.push(Span::styled(trimmed, message_style));
            lines.push(Line::from(spans));
        }

        let muted_style = self.styles.default_style().add_modifier(Modifier::DIM);
        let hidden = self.queued_inputs.len().saturating_sub(VISIBLE_ITEMS);
        if hidden > 0 {
            lines.push(Line::from(vec![Span::styled(format!("+{hidden} more queued"), muted_style)]));
        }
        lines.push(Line::from(vec![Span::styled(
            super::terminal_capabilities::queued_input_edit_hint().to_string(),
            muted_style,
        )]));

        lines
    }

    fn invalidate_queue_overlay(&mut self) {
        self.queue_overlay_version = self.queue_overlay_version.wrapping_add(1);
        self.queue_overlay_cache = None;
        self.request_transcript_clear();
    }

    fn queue_overlay_lines(&mut self, width: u16) -> Option<&[Line<'static>]> {
        if width == 0 || self.queued_inputs.is_empty() {
            self.queue_overlay_cache = None;
            return None;
        }

        let version = self.queue_overlay_version;
        let needs_rebuild = match &self.queue_overlay_cache {
            Some(cache) => cache.width != width || cache.version != version,
            None => true,
        };

        if needs_rebuild {
            let lines = self.reflow_queue_lines(width);
            self.queue_overlay_cache = Some(QueueOverlay { width, version, lines });
        }

        self.queue_overlay_cache.as_ref().and_then(|cache| {
            if cache.lines.is_empty() {
                None
            } else {
                Some(cache.lines.as_slice())
            }
        })
    }

    pub(crate) fn overlay_queue_lines(&mut self, visible_lines: &mut [TranscriptLine], content_width: u16) {
        if visible_lines.is_empty() || content_width == 0 {
            return;
        }

        if let Some(queue_lines) = self.queue_overlay_lines(content_width) {
            let queue_visible = queue_lines.len().min(visible_lines.len());
            let start = visible_lines.len().saturating_sub(queue_visible);
            let slice_start = queue_lines.len().saturating_sub(queue_visible);
            let overlay = &queue_lines[slice_start..];
            for (target, source) in visible_lines[start..].iter_mut().zip(overlay.iter()) {
                *target = TranscriptLine { line: source.clone(), explicit_links: Vec::new() };
            }
        }
    }

    fn reflow_queue_lines(&self, width: u16) -> Vec<Line<'static>> {
        self.queue_input_lines(width)
    }
}

fn truncate_to_width(text: &str, max_width: usize) -> String {
    preview::truncate_with_ellipsis(text, max_width, "…")
}
