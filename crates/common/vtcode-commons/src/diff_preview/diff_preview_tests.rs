//! Extracted regression tests; production source remains byte-identical.

use super::*;
use crate::diff::{DiffLine, DiffLineKind};

#[test]
fn counts_diff_changes_from_hunks() {
    let hunks = vec![DiffHunk {
        old_start: 1,
        old_lines: 2,
        new_start: 1,
        new_lines: 2,
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(1),
                new_line: Some(1),
                text: "same\n".to_string(),
            },
            DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(2),
                new_line: None,
                text: "old\n".to_string(),
            },
            DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(2),
                text: "new\n".to_string(),
            },
        ],
    }];

    let counts = count_diff_changes(&hunks);
    assert_eq!(counts.additions, 1);
    assert_eq!(counts.deletions, 1);
    assert_eq!(counts.total(), 2);
}

#[test]
fn formats_numbered_unified_diff_with_start_only_headers() {
    let diff = "\
diff --git a/file.txt b/file.txt
@@ -10,2 +10,2 @@
-old
+new
 context
";

    let lines = format_numbered_unified_diff(diff);
    assert_eq!(lines[0], "diff --git a/file.txt b/file.txt");
    assert!(lines.iter().any(|line| line == "@@ -10 +10 @@"));
    assert!(lines.iter().any(|line| line.starts_with("-   10 │ old")));
    assert!(lines.iter().any(|line| line.starts_with("+   10 │ new")));
    assert!(lines.iter().any(|line| line.starts_with("    11 │ context")));
}

#[test]
fn numbered_text_uses_pipe_separator_for_markdown_bullets() {
    let line = DiffDisplayLine {
        kind: DiffDisplayKind::Addition,
        line_number: Some(53),
        text: "- **Agent-first by design**: prose".to_string(),
    };
    assert_eq!(line.numbered_text(5), "+   53 │ - **Agent-first by design**: prose");
}

#[test]
fn display_lines_from_hunks_preserves_semantics() {
    let hunks = vec![DiffHunk {
        old_start: 10,
        old_lines: 2,
        new_start: 10,
        new_lines: 2,
        lines: vec![
            DiffLine {
                kind: DiffLineKind::Deletion,
                old_line: Some(10),
                new_line: None,
                text: "old\n".to_string(),
            },
            DiffLine {
                kind: DiffLineKind::Addition,
                old_line: None,
                new_line: Some(10),
                text: "new\n".to_string(),
            },
            DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(11),
                new_line: Some(11),
                text: "same\n".to_string(),
            },
        ],
    }];

    let lines = display_lines_from_hunks(&hunks);
    assert_eq!(lines[0].kind, DiffDisplayKind::HunkHeader);
    assert_eq!(lines[0].text, "@@ -10 +10 @@");
    assert_eq!(lines[1].kind, DiffDisplayKind::Deletion);
    assert_eq!(lines[1].line_number, Some(10));
    assert_eq!(lines[1].text, "old");
    assert_eq!(lines[2].kind, DiffDisplayKind::Addition);
    assert_eq!(lines[2].line_number, Some(10));
    assert_eq!(lines[3].kind, DiffDisplayKind::Context);
    assert_eq!(lines[3].line_number, Some(11));
}

#[test]
fn diff_display_line_number_width_tracks_max_digits() {
    let lines = vec![
        DiffDisplayLine {
            kind: DiffDisplayKind::Addition,
            line_number: Some(99),
            text: "let a = 1;".to_string(),
        },
        DiffDisplayLine {
            kind: DiffDisplayKind::Context,
            line_number: Some(10_420),
            text: "let b = 2;".to_string(),
        },
    ];

    assert_eq!(diff_display_line_number_width(&lines), 5);
}

#[test]
fn preserves_plain_text_when_not_diff() {
    let lines = format_numbered_unified_diff("plain text output");
    assert_eq!(lines, vec!["plain text output".to_string()]);
}

#[test]
fn is_diff_discriminates_content_lines() {
    let diff = "\
diff --git a/file.txt b/file.txt
@@ -1 +1 @@
-old
+new
 context
";
    let lines = display_lines_from_unified_diff(diff);
    assert_eq!(lines[0].kind, DiffDisplayKind::Metadata);
    assert!(!lines[0].is_diff());
    assert_eq!(lines[1].kind, DiffDisplayKind::HunkHeader);
    assert!(!lines[1].is_diff());
    assert!(lines[2].is_diff());
    assert!(lines[3].is_diff());
    assert!(lines[4].is_diff());
}

#[test]
fn omitted_marker_advances_both_counters() {
    let diff = "\
@@ -1,5 +1,5 @@
-one
... 3 lines omitted ...
 old tail
";

    let lines = display_lines_from_unified_diff(diff);
    assert_eq!(lines[1].kind, DiffDisplayKind::Deletion);
    assert_eq!(lines[1].line_number, Some(1));
    assert_eq!(lines[2].kind, DiffDisplayKind::Metadata);
    assert_eq!(lines[2].line_number, None);
    assert_eq!(lines[3].kind, DiffDisplayKind::Context);
    assert_eq!(lines[3].line_number, Some(4));
}

#[test]
fn diff_display_line_number_width_clamps_to_bounds() {
    let small = vec![DiffDisplayLine {
        kind: DiffDisplayKind::Context,
        line_number: Some(1),
        text: "text".to_string(),
    }];
    assert_eq!(diff_display_line_number_width(&small), 5);

    let large = vec![DiffDisplayLine {
        kind: DiffDisplayKind::Context,
        line_number: Some(100_000),
        text: "text".to_string(),
    }];
    assert_eq!(diff_display_line_number_width(&large), 6);
}

#[test]
fn metadata_lines_stay_metadata_after_hunk() {
    let diff = "\
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
";

    let lines = display_lines_from_unified_diff(diff);
    assert_eq!(lines[2].kind, DiffDisplayKind::Metadata);
    assert_eq!(lines[2].line_number, None);
    assert_eq!(lines[3].kind, DiffDisplayKind::Addition);
    assert_eq!(lines[3].line_number, Some(1));
}
