use super::layout::{BottomPanelKind, resolve_bottom_panel_spec, split_input_and_bottom_panel_area};
use super::task_panel;
use super::*;
use crate::tui::config::constants::ui;
use crate::tui::core_tui::app::session::transient::TransientSurface;
use crate::tui::core_tui::session::render as core_render;
use crate::tui::core_tui::session::{list_panel, message_renderer};
use ratatui::{buffer::Buffer, style::Modifier};

impl Session {
    pub fn render(&mut self, frame: &mut Frame<'_>) {
        let Some(viewport) = self.core.begin_frame(frame) else {
            return;
        };
        let mut metrics = self.core.measure_frame(viewport);
        // The slash palette renders its own focused search field with a visible
        // cursor. Keeping the base input rendered alongside it produces a double
        // input + double cursor, so hand the full input region to the panel and
        // suppress the base input (and its status line) while it is open.
        let panel_captures_input = self.inline_lists_visible()
            && matches!(
                self.visible_bottom_docked_surface(),
                Some(TransientSurface::LocalAgents) | Some(TransientSurface::SlashPalette)
            );
        let panel = resolve_bottom_panel_spec(
            self,
            viewport,
            metrics.header_height,
            if panel_captures_input {
                0
            } else {
                metrics.input_core_height
            },
        );
        if panel_captures_input {
            metrics.input_core_height = 0;
        }
        let layout = self.core.build_frame_layout(viewport, metrics, panel.height);
        self.core.set_modal_list_area(None);
        let modal_area = self
            .has_active_overlay()
            .then(|| core_render::floating_modal_area(layout.viewport));
        let transcript_area = modal_area
            .map_or(layout.main_area, |modal_area| core_render::clip_transcript_area(layout.main_area, modal_area));
        let (input_area, bottom_panel_area) =
            if matches!(panel.kind, BottomPanelKind::LocalAgents | BottomPanelKind::SlashPalette) {
                (
                    Rect::new(layout.input_area.x, layout.input_area.y, layout.input_area.width, 0),
                    Some(layout.input_area),
                )
            } else {
                split_input_and_bottom_panel_area(layout.input_area, panel.height)
            };
        self.core.set_bottom_panel_area(bottom_panel_area);
        self.core.render_base_frame(frame, &layout, transcript_area);
        {
            let buffer = &*frame.buffer_mut();
            self.rebuild_compact_activity_hit_regions(buffer, transcript_area);
        }
        self.core.render_input(frame, input_area);
        if let Some(panel_area) = bottom_panel_area {
            match panel.kind {
                BottomPanelKind::AgentPalette => {
                    render::render_agent_palette(self, frame, panel_area);
                }
                BottomPanelKind::FilePalette => {
                    render::render_file_palette(self, frame, panel_area);
                }
                BottomPanelKind::HistoryPicker => {
                    render::render_history_picker(self, frame, panel_area);
                }
                BottomPanelKind::SlashPalette => {
                    slash::render_slash_palette(self, frame, panel_area);
                }
                BottomPanelKind::TaskPanel => {
                    render_task_panel(self, frame, panel_area);
                }
                BottomPanelKind::LocalAgents => {
                    render::render_local_agents(self, frame, panel_area);
                }
                BottomPanelKind::None => {
                    frame.render_widget(Clear, panel_area);
                }
            }
        }

        if let Some(modal_area) = modal_area {
            core_render::render_modal(self, frame, modal_area);
        }

        if self.diff_preview_state().is_some() {
            diff_preview::render_diff_preview(self, frame, layout.viewport);
        }
        if let Some(mut state) = self.tool_output_viewer_state.take() {
            let width = tool_output_viewer::viewer_content_width(layout.viewport);
            let height = tool_output_viewer::viewer_content_height(self, &state, layout.viewport);
            state.refresh(self, width, height);
            tool_output_viewer::render_tool_output_viewer(self, frame, layout.viewport, &mut state);
            self.tool_output_viewer_state = Some(state);
        }
        self.core.finalize_mouse_selection(frame, layout.viewport);
    }

    #[expect(
        dead_code,
        reason = "Intentional compatibility, platform, test, or API-shape suppression."
    )]
    fn render_message_spans(&self, index: usize) -> Vec<Span<'static>> {
        let Some(line) = self.core.lines.get(index) else {
            return vec![Span::raw(String::new())];
        };
        message_renderer::render_message_spans(
            line,
            &self.core.theme,
            &self.core.labels,
            |kind| self.core.prefix_text(kind),
            |line| self.core.prefix_style(line),
            |kind| self.core.text_fallback(kind),
        )
    }
}

impl Session {
    fn rebuild_compact_activity_hit_regions(&mut self, buffer: &Buffer, area: Rect) {
        self.compact_activity_hit_regions.clear();
        if tool_output_viewer::compact_activity_hint_text(self).is_none() {
            return;
        }
        if area.width == 0 || area.height == 0 || self.core.transcript_width == 0 {
            return;
        }

        let activity_ranges = self
            .compact_activity_entries
            .iter()
            .filter_map(|entry| entry.metadata.review_anchor.map(|anchor| (entry.line_index, anchor)))
            .collect::<Vec<_>>();
        let transcript_width = self.core.transcript_width;
        let view_top = self.core.transcript_view_top;

        for (line_index, review_anchor) in activity_ranges {
            let Some((start_row, end_row)) = self.core.transcript_message_row_range(transcript_width, line_index)
            else {
                continue;
            };
            for transcript_row in start_row..end_row {
                let Some(screen_row) = transcript_row
                    .checked_sub(view_top)
                    .and_then(|row| u16::try_from(row).ok())
                    .and_then(|row| area.y.checked_add(row))
                else {
                    continue;
                };
                if screen_row >= area.bottom() {
                    continue;
                }
                for hit_area in find_underlined_text_regions(buffer, area, screen_row) {
                    self.compact_activity_hit_regions
                        .push(CompactActivityHitRegion { area: hit_area, review_anchor });
                }
            }
        }
    }
}

fn find_underlined_text_regions(buffer: &Buffer, area: Rect, row: u16) -> Vec<Rect> {
    if area.width == 0 || area.height == 0 || row < area.y || row >= area.bottom() {
        return Vec::new();
    }

    let mut regions = Vec::new();
    let mut start = None;
    for column in area.x..area.right() {
        let underlined = buffer[(column, row)].style().add_modifier.contains(Modifier::UNDERLINED);
        match (start, underlined) {
            (None, true) => start = Some(column),
            (Some(start_column), false) => {
                regions.push(Rect::new(start_column, row, column.saturating_sub(start_column), 1));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(start_column) = start {
        regions.push(Rect::new(start_column, row, area.right().saturating_sub(start_column), 1));
    }
    regions
}

fn render_task_panel(session: &mut Session, frame: &mut Frame<'_>, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let panel_lines = task_panel::body_lines(&session.task_panel_lines, session.task_panel_metadata.as_ref());
    let rows = task_panel::rows(panel_lines, area.width, session.core.header_secondary_style());
    let item_count = panel_lines.len();
    let (title, progress) = task_panel::header(session.task_panel_metadata.as_ref(), item_count);
    let sections = list_panel::SharedListPanelSections {
        header: vec![Line::from(vec![Span::styled(
            title.to_string(),
            session.core.section_title_style(),
        )])],
        info: vec![Line::from(progress)],
        search: None,
    };
    let styles = list_panel::SharedListPanelStyles {
        base_style: session.core.styles.default_style(),
        selected_style: Some(session.core.styles.modal_list_highlight_style()),
        text_style: session.core.styles.default_style(),
        divider_style: None,
        input_styles: list_panel::input_styles_from_theme(&session.core.theme),
        show_divider: false,
    };
    let mut model = list_panel::StaticRowsListPanelModel {
        rows,
        selected: None,
        offset: 0,
        visible_rows: area.height as usize,
    };
    list_panel::render_shared_list_panel(frame, area, sections, styles, &mut model);
}
