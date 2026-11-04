use ratatui::{prelude::*, widgets::Clear};

use super::Session;

impl Session {
    #[expect(
        dead_code,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    fn render_navigation(&mut self, frame: &mut Frame<'_>, area: Rect) {
        frame.render_widget(Clear, area);
        // Navigation/ Timeline pane has been removed
    }
}
