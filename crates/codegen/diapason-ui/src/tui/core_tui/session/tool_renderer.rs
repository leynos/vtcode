use ratatui::prelude::*;

use super::super::style::ratatui_style_from_inline;
use super::super::types::InlineTheme;
use super::message::MessageLine;

#[expect(
    dead_code,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
fn render_tool_segments(
    line: &MessageLine,
    theme: &InlineTheme,
    _strip_ansi_fn: impl Fn(&str) -> String,
) -> Vec<Span<'static>> {
    // Render tool output without header decorations - just display segments directly
    let mut spans = Vec::new();
    for segment in &line.segments {
        let style = ratatui_style_from_inline(&segment.style, theme.foreground);
        spans.push(Span::styled(segment.text.clone(), style));
    }
    spans
}
