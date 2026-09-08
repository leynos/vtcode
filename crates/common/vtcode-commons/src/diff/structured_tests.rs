//! Extracted regression tests; production source remains byte-identical.

use super::*;

// ── compute_diff_chunks ──────────────────────────────────────────

// ── compute_diff ─────────────────────────────────────────────────

fn identity_formatter(hunks: &[DiffHunk], _opts: &DiffOptions<'_>) -> String {
    hunks
        .iter()
        .flat_map(|h| h.lines.iter().map(|l| l.text.clone()))
        .collect::<Vec<_>>()
        .join("")
}

#[test]
fn diff_identical_content() {
    let result = compute_diff("hello\n", "hello\n", DiffOptions::default(), identity_formatter);
    assert!(result.is_empty);
    assert!(result.hunks.is_empty());
    assert!(result.formatted.is_empty());
}

#[test]
fn diff_empty_both() {
    let result = compute_diff("", "", DiffOptions::default(), identity_formatter);
    assert!(result.is_empty);
    assert!(result.hunks.is_empty());
}

#[test]
fn diff_old_empty() {
    let result = compute_diff("", "line1\nline2\n", DiffOptions::default(), identity_formatter);
    assert!(!result.is_empty);
    assert!(!result.hunks.is_empty());
    // All lines should be additions
    for hunk in &result.hunks {
        for line in &hunk.lines {
            assert_eq!(line.kind, DiffLineKind::Addition);
        }
    }
}

#[test]
fn diff_new_empty() {
    let result = compute_diff("line1\nline2\n", "", DiffOptions::default(), identity_formatter);
    assert!(!result.is_empty);
    assert!(!result.hunks.is_empty());
    for hunk in &result.hunks {
        for line in &hunk.lines {
            assert_eq!(line.kind, DiffLineKind::Deletion);
        }
    }
}

#[test]
fn diff_single_line_change() {
    let old = "aaa\nbbb\nccc\n";
    let new = "aaa\nxxx\nccc\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    assert!(!result.is_empty);
    assert_eq!(result.hunks.len(), 1);

    let hunk = &result.hunks[0];
    // Should have context lines for aaa and ccc, plus the change
    let kinds: Vec<DiffLineKind> = hunk.lines.iter().map(|l| l.kind).collect();
    assert!(kinds.contains(&DiffLineKind::Context));
    assert!(kinds.contains(&DiffLineKind::Deletion));
    assert!(kinds.contains(&DiffLineKind::Addition));
}

#[test]
fn diff_line_numbers() {
    let old = "line1\nline2\nline3\n";
    let new = "line1\nline2 modified\nline3\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    let hunk = &result.hunks[0];
    // Context lines should have both old_line and new_line
    for line in &hunk.lines {
        if line.kind == DiffLineKind::Context {
            assert!(line.old_line.is_some());
            assert!(line.new_line.is_some());
        }
    }
    // Deletion should have old_line but no new_line
    for line in &hunk.lines {
        if line.kind == DiffLineKind::Deletion {
            assert!(line.old_line.is_some());
            assert!(line.new_line.is_none());
        }
    }
    // Addition should have new_line but no old_line
    for line in &hunk.lines {
        if line.kind == DiffLineKind::Addition {
            assert!(line.old_line.is_none());
            assert!(line.new_line.is_some());
        }
    }
}

#[test]
fn diff_context_lines_zero() {
    let old = "a\nb\nc\nd\ne\n";
    let new = "a\nb\nX\nd\ne\n";
    let opts = DiffOptions { context_lines: 0, ..DiffOptions::default() };
    let result = compute_diff(old, new, opts, identity_formatter);

    assert!(!result.is_empty);
    // With 0 context, only the changed line and its neighbours should appear
    let hunk = &result.hunks[0];
    // Should be minimal: just the deletion and addition
    let context_count = hunk.lines.iter().filter(|l| l.kind == DiffLineKind::Context).count();
    assert!(context_count <= 2); // At most one context line on each side
}

#[test]
fn diff_context_lines_large() {
    let old = "a\nb\nc\nd\ne\n";
    let new = "a\nb\nX\nd\ne\n";
    let opts = DiffOptions { context_lines: 10, ..DiffOptions::default() };
    let result = compute_diff(old, new, opts, identity_formatter);

    assert!(!result.is_empty);
    // With 10 context lines and only 6 total lines (trailing newline creates 6th), all lines appear
    let hunk = &result.hunks[0];
    assert_eq!(hunk.lines.len(), 6);
}

#[test]
fn diff_hunk_metadata() {
    let old = "aaa\nbbb\nccc\n";
    let new = "aaa\nxxx\nccc\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    let hunk = &result.hunks[0];
    assert!(hunk.old_start >= 1);
    assert!(hunk.new_start >= 1);
    assert!(hunk.old_lines > 0);
    assert!(hunk.new_lines > 0);
    assert!(!hunk.lines.is_empty());
}

#[test]
fn diff_hunk_start_uses_the_first_represented_line() {
    let result = compute_diff("existing\n", "inserted\nexisting\n", DiffOptions::default(), identity_formatter);
    let hunk = &result.hunks[0];

    assert_eq!(hunk.old_start, 1);
    assert_eq!(hunk.new_start, 1);
    assert_eq!(hunk.old_lines, 1);
    assert_eq!(hunk.new_lines, 2);
}

#[test]
fn diff_multiple_hunks() {
    // Insert in first half and insert in second half with small context => two hunks
    let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
    let new = "a\nINSERTED1\nb\nc\nd\ne\nf\ng\nINSERTED2\nh\n";
    let opts = DiffOptions { context_lines: 1, ..DiffOptions::default() };
    let result = compute_diff(old, new, opts, identity_formatter);

    assert!(!result.is_empty);
    assert!(result.hunks.len() >= 2, "expected at least 2 hunks, got {}", result.hunks.len());
}

#[test]
fn diff_formatter_called() {
    let old = "aaa\n";
    let new = "bbb\n";
    let mut called = false;
    let formatter = |hunks: &[DiffHunk], _opts: &DiffOptions<'_>| -> String {
        called = true;
        hunks
            .iter()
            .flat_map(|h| h.lines.iter().map(|l| l.text.clone()))
            .collect::<Vec<_>>()
            .join("")
    };

    let result = compute_diff(old, new, DiffOptions::default(), formatter);
    assert!(called);
    assert!(!result.formatted.is_empty());
}

#[test]
fn diff_formatter_not_called_when_empty() {
    let mut called = false;
    let formatter = |_hunks: &[DiffHunk], _opts: &DiffOptions<'_>| -> String {
        called = true;
        String::new()
    };

    let result = compute_diff("same\n", "same\n", DiffOptions::default(), formatter);
    assert!(!called);
    assert!(result.formatted.is_empty());
}

#[test]
fn diff_options_labels() {
    let old = "aaa\n";
    let new = "bbb\n";
    let opts = DiffOptions {
        old_label: Some("old.txt"),
        new_label: Some("new.txt"),
        ..DiffOptions::default()
    };
    let result = compute_diff(old, new, opts, identity_formatter);
    assert!(!result.is_empty);
    // Labels are passed to formatter but don't affect hunks
    assert_eq!(result.hunks.len(), 1);
}

#[test]
fn diff_insertion_only() {
    let old = "line1\nline3\n";
    let new = "line1\nline2\nline3\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    assert!(!result.is_empty);
    let additions: Vec<&DiffLine> = result
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Addition)
        .collect();
    assert_eq!(additions.len(), 1);
    assert_eq!(additions[0].text, "line2\n");
}

#[test]
fn diff_preserves_crlf_and_cr_line_endings() {
    let crlf = compute_diff("one\r\ntwo\r\n", "one\r\nchanged\r\n", DiffOptions::default(), identity_formatter);
    assert_eq!(
        crlf.hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (DiffLineKind::Context, "one\r\n"),
            (DiffLineKind::Deletion, "two\r\n"),
            (DiffLineKind::Addition, "changed\r\n"),
        ]
    );

    let cr = compute_diff("one\rtwo\r", "one\rchanged\r", DiffOptions::default(), identity_formatter);
    assert_eq!(
        cr.hunks[0]
            .lines
            .iter()
            .map(|line| (line.kind, line.text.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (DiffLineKind::Context, "one\r"),
            (DiffLineKind::Deletion, "two\r"),
            (DiffLineKind::Addition, "changed\r"),
        ]
    );
}

#[test]
fn diff_deletion_only() {
    let old = "line1\nline2\nline3\n";
    let new = "line1\nline3\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    assert!(!result.is_empty);
    let deletions: Vec<&DiffLine> = result
        .hunks
        .iter()
        .flat_map(|h| h.lines.iter())
        .filter(|l| l.kind == DiffLineKind::Deletion)
        .collect();
    assert_eq!(deletions.len(), 1);
    assert_eq!(deletions[0].text, "line2\n");
}

#[test]
fn diff_bundle_serializes() {
    let old = "aaa\nbbb\n";
    let new = "aaa\nxxx\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);

    let json = serde_json::to_value(&result).expect("diff bundle fixture must serialize");
    assert_eq!(json.get("hunks").and_then(serde_json::Value::as_array).map(Vec::len), Some(1));
    assert_eq!(json.get("formatted").and_then(serde_json::Value::as_str), Some(result.formatted.as_str()));
    assert_eq!(json.get("is_empty").and_then(serde_json::Value::as_bool), Some(false));
}

#[test]
fn diff_hunk_serializes() {
    let hunk = DiffHunk {
        old_start: 1,
        old_lines: 2,
        new_start: 1,
        new_lines: 2,
        lines: vec![DiffLine {
            kind: DiffLineKind::Context,
            old_line: Some(1),
            new_line: Some(1),
            text: "hello\n".to_string(),
        }],
    };
    let json = serde_json::to_value(&hunk).expect("diff hunk fixture must serialize");
    assert_eq!(
        json,
        serde_json::json!({
            "old_start": 1,
            "old_lines": 2,
            "new_start": 1,
            "new_lines": 2,
            "lines": [{"kind": "context", "old_line": 1, "new_line": 1, "text": "hello\n"}],
        })
    );
}

#[test]
fn diff_line_kind_serializes() {
    for (kind, wire_name) in [
        (DiffLineKind::Context, "context"),
        (DiffLineKind::Addition, "addition"),
        (DiffLineKind::Deletion, "deletion"),
    ] {
        let encoded = serde_json::to_value(kind).expect("diff line kind fixture must serialize");
        assert_eq!(encoded.as_str(), Some(wire_name));
    }
}

#[test]
fn chunks_very_long_identical() {
    let text = "x".repeat(10_000);
    let chunks = compute_diff_chunks(&text, &text);
    assert_eq!(chunks.len(), 1);
    assert!(matches!(chunks[0], Chunk::Equal(_)));
}

#[test]
fn chunks_single_char_diff() {
    let chunks = compute_diff_chunks("a", "b");
    assert!(!chunks.is_empty());
    let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
    let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
    assert!(has_delete && has_insert);
}

#[test]
fn diff_no_trailing_newline() {
    let old = "line1\nline2";
    let new = "line1\nline2\n";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);
    assert!(!result.is_empty);
}

#[test]
fn diff_only_newlines_differ() {
    let old = "a\nb\n";
    let new = "a\nb";
    let result = compute_diff(old, new, DiffOptions::default(), identity_formatter);
    assert!(!result.is_empty);
}

#[test]
fn chunks_prefix_suffix_optimization() {
    // Verify that common prefix and suffix are preserved as Equal chunks.
    // Myers works character-by-character, so the middle diff is char-level.
    let old = "AAAA BBBB CCCC";
    let new = "AAAA DDDD CCCC";
    let chunks = compute_diff_chunks(old, new);

    // First chunk should be Equal prefix "AAAA "
    assert!(matches!(&chunks[0], Chunk::Equal(s) if *s == "AAAA "));
    // Last chunk should be Equal suffix " CCCC"
    assert!(matches!(chunks.last().unwrap(), Chunk::Equal(s) if *s == " CCCC"));
    // Middle should contain deletes and inserts (character-level)
    let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
    let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
    assert!(has_delete, "expected Delete chunks in middle");
    assert!(has_insert, "expected Insert chunks in middle");
}
