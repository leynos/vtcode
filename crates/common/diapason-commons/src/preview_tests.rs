//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn truncate_to_display_width_respects_wide_chars() {
    let value = "表表表";
    assert_eq!(truncate_to_display_width(value, 5), "表表");
}

#[test]
fn truncate_with_ellipsis_respects_width_budget() {
    assert_eq!(truncate_with_ellipsis("abcdef", 4, "…"), "abc…");
}

#[test]
fn pad_to_display_width_handles_wide_chars() {
    let padded = pad_to_display_width("表", 4, ' ');
    assert_eq!(display_width(padded.as_str()), 4);
}

#[test]
fn suffix_for_display_width_preserves_tail() {
    assert_eq!(suffix_for_display_width("hello/world.rs", 8), "world.rs");
}

#[test]
fn split_head_tail_preview_preserves_hidden_count() {
    let items = [1, 2, 3, 4, 5, 6, 7];
    let preview = split_head_tail_preview(&items, 2, 2);
    assert_eq!(preview.head, &[1, 2]);
    assert_eq!(preview.tail, &[6, 7]);
    assert_eq!(preview.hidden_count, 3);
    assert_eq!(preview.total, 7);
}

#[test]
fn split_head_tail_preview_keeps_short_input_intact() {
    let items = [1, 2, 3];
    let preview = split_head_tail_preview(&items, 2, 2);
    assert_eq!(preview.head, &[1, 2, 3]);
    assert!(preview.tail.is_empty());
    assert_eq!(preview.hidden_count, 0);
}

#[test]
fn split_head_tail_preview_with_limit_preserves_total_and_gap() {
    let items = [1, 2, 3, 4, 5, 6, 7];
    let preview = split_head_tail_preview_with_limit(&items, 6, 3);
    assert_eq!(preview.head, &[1, 2]);
    assert_eq!(preview.tail, &[5, 6, 7]);
    assert_eq!(preview.hidden_count, 2);
    assert_eq!(preview.total, 7);
}

#[test]
fn summary_window_reserves_gap_row() {
    assert_eq!(summary_window(6, 3), (2, 3));
    assert_eq!(summary_window(2, 3), (0, 2));
}

#[test]
fn hidden_lines_summary_matches_existing_copy() {
    assert_eq!(format_hidden_lines_summary(1), "… +1 line");
    assert_eq!(format_hidden_lines_summary(4), "… +4 lines");
}

#[test]
fn excerpt_text_lines_builds_head_tail_vectors() {
    let preview = excerpt_text_lines("l1\nl2\nl3\nl4\nl5\nl6", 2, 2);
    assert_eq!(preview.head, vec!["l1", "l2"]);
    assert_eq!(preview.tail, vec!["l5", "l6"]);
    assert_eq!(preview.hidden_count, 2);
    assert_eq!(preview.total, 6);
}

#[test]
fn condense_text_bytes_respects_utf8_boundaries() {
    let mut content = "a".repeat(7);
    content.push('é');
    content.push_str("bbbbbbbb");

    let preview = condense_text_bytes(&content, 8, 4);
    assert!(preview.contains("bytes omitted"));
    assert!(preview.is_char_boundary(0));
}

#[test]
fn tail_preview_text_keeps_last_lines_only() {
    let input = (0..20).map(|index| format!("line-{index}")).collect::<Vec<_>>().join("\n");

    let preview = tail_preview_text(&input, 40, 3);
    assert!(preview.contains("bytes omitted"));
    assert!(preview.contains("line-19"));
    assert!(!preview.contains("line-1\n"));
}
