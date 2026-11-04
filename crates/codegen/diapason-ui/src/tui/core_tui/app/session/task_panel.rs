use ratatui::prelude::*;

use super::super::super::session::{inline_list, text_utils};
use super::super::types::TaskPanelMetadata;
use crate::tui::config::constants::ui;

pub(super) fn compact_tree_continuation_prefix(line: &str) -> Option<String> {
    text_utils::compact_tree_continuation_prefix(line)
}

pub(super) fn wrap_line(line: &str, width: u16) -> Vec<Line<'static>> {
    if width == 0 {
        return vec![Line::from(line.to_string())];
    }

    let content = Line::from(line.to_string());
    if let Some(prefix) = compact_tree_continuation_prefix(line) {
        text_utils::wrap_line_with_hanging_prefix(content, width as usize, &prefix)
    } else {
        text_utils::wrap_line(content, width as usize)
    }
}

pub(super) fn rows(lines: &[String], width: u16, text_style: Style) -> Vec<(inline_list::InlineListRow, u16)> {
    if lines.is_empty() {
        return vec![(inline_list::InlineListRow::single(ui::PLAN_STATUS_EMPTY.to_string().into(), text_style), 1)];
    }

    lines
        .iter()
        .map(|line| {
            let wrapped = wrap_line(line, width);
            let height = inline_list::row_height(&wrapped);
            (inline_list::InlineListRow { lines: wrapped, style: text_style }, height)
        })
        .collect()
}

pub(super) fn body_lines<'a>(lines: &'a [String], metadata: Option<&TaskPanelMetadata>) -> &'a [String] {
    let Some(metadata) = metadata else {
        return lines;
    };

    let Some(first) = lines.first() else {
        return lines;
    };

    first
        .strip_prefix("• ")
        .filter(|title| *title == metadata.title)
        .map_or(lines, |_| &lines[1..])
}

pub(super) fn row_count(lines: &[String], width: u16) -> usize {
    rows(lines, width, Style::default())
        .iter()
        .map(|(_, height)| *height as usize)
        .sum::<usize>()
        .max(1)
}

pub(super) fn header(metadata: Option<&TaskPanelMetadata>, item_count: usize) -> (String, String) {
    let title = metadata
        .map(|metadata| metadata.title.as_str())
        .filter(|title| !title.trim().is_empty())
        .unwrap_or(ui::PLAN_BLOCK_TITLE)
        .to_string();
    let progress = metadata
        .map(|metadata| format!("{}/{} complete", metadata.completed, metadata.total))
        .unwrap_or_else(|| format!("{} item{}", item_count, if item_count == 1 { "" } else { "s" }));
    (title, progress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(row: &inline_list::InlineListRow) -> Vec<String> {
        row.lines
            .iter()
            .map(|line| line.spans.iter().map(|span| span.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn compact_tree_continuations_hang_two_spaces_beyond_tree_indent() {
        assert_eq!(compact_tree_continuation_prefix("  └ Parent"), Some("    ".to_string()));
        assert_eq!(compact_tree_continuation_prefix("    [-] Child"), Some("      ".to_string()));
        assert_eq!(compact_tree_continuation_prefix("• Updated Plan"), None);
    }

    #[test]
    fn wrapped_tree_rows_keep_hanging_indentation() {
        let rows = rows(&["    [-] A child task with a long description".to_string()], 18, Style::default());
        assert_eq!(rows.len(), 1);
        assert!(row_text(&rows[0].0)[1].starts_with("      "));
        assert_eq!(rows[0].1, 4);
    }

    #[test]
    fn row_count_accounts_for_wrapped_tree_rows() {
        let lines = vec![
            "  └ A short parent".to_string(),
            "    □ A much longer child task".to_string(),
        ];
        assert_eq!(row_count(&lines, 80), 2);
        assert!(row_count(&lines, 12) > 2);
    }

    #[test]
    fn header_uses_typed_title_and_completion_progress() {
        let metadata = TaskPanelMetadata {
            title: "Release".to_string(),
            completed: 2,
            total: 5,
        };
        assert_eq!(header(Some(&metadata), 8), ("Release".to_string(), "2/5 complete".to_string()));
        assert_eq!(header(None, 1), (ui::PLAN_BLOCK_TITLE.to_string(), "1 item".to_string()));
    }

    #[test]
    fn body_lines_uses_typed_header_instead_of_duplicate_title_row() {
        let lines = vec!["• Release".to_string(), "  └ Prepare release".to_string()];
        let metadata = TaskPanelMetadata {
            title: "Release".to_string(),
            completed: 0,
            total: 1,
        };

        assert_eq!(body_lines(&lines, Some(&metadata)), &lines[1..]);
        assert_eq!(body_lines(&lines, None), &lines[..]);
    }

    #[test]
    fn rows_keep_the_existing_empty_state() {
        let empty_rows = rows(&[], 40, Style::default());
        assert_eq!(empty_rows.len(), 1);
        assert_eq!(row_text(&empty_rows[0].0), vec![ui::PLAN_STATUS_EMPTY.to_string()]);
        assert_eq!(empty_rows[0].1, 1);
    }
}
