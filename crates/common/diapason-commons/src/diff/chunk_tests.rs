//! Extracted regression tests; production source remains byte-identical.

use super::*;

// ── compute_diff_chunks ──────────────────────────────────────────

#[test]
fn chunks_both_empty() {
    let chunks = compute_diff_chunks("", "");
    assert!(chunks.is_empty());
}

#[test]
fn chunks_old_empty() {
    let chunks = compute_diff_chunks("", "hello");
    assert_eq!(chunks, vec![Chunk::Insert("hello")]);
}

#[test]
fn chunks_new_empty() {
    let chunks = compute_diff_chunks("hello", "");
    assert_eq!(chunks, vec![Chunk::Delete("hello")]);
}

#[test]
fn chunks_identical() {
    let chunks = compute_diff_chunks("abc", "abc");
    assert_eq!(chunks.len(), 1);
    assert!(matches!(chunks[0], Chunk::Equal("abc")));
}

#[test]
fn chunks_single_insertion() {
    let chunks = compute_diff_chunks("ac", "abc");
    // Common prefix "a", insert "b", common suffix "c"
    assert_eq!(chunks.len(), 3);
    assert!(matches!(chunks[0], Chunk::Equal("a")));
    assert!(matches!(chunks[1], Chunk::Insert("b")));
    assert!(matches!(chunks[2], Chunk::Equal("c")));
}

#[test]
fn chunks_single_deletion() {
    let chunks = compute_diff_chunks("abc", "ac");
    assert_eq!(chunks.len(), 3);
    assert!(matches!(chunks[0], Chunk::Equal("a")));
    assert!(matches!(chunks[1], Chunk::Delete("b")));
    assert!(matches!(chunks[2], Chunk::Equal("c")));
}

#[test]
fn chunks_replacement() {
    let chunks = compute_diff_chunks("abc", "axc");
    // Equal("a"), Delete("b"), Insert("x"), Equal("c")
    assert_eq!(chunks.len(), 4);
    assert!(matches!(chunks[0], Chunk::Equal("a")));
    assert!(matches!(chunks[1], Chunk::Delete("b")));
    assert!(matches!(chunks[2], Chunk::Insert("x")));
    assert!(matches!(chunks[3], Chunk::Equal("c")));
}

#[test]
fn chunks_completely_different() {
    let chunks = compute_diff_chunks("aaa", "bbb");
    // No common prefix or suffix
    assert!(!chunks.is_empty());
    // All old chars deleted, all new chars inserted
    let deletes: usize = chunks.iter().filter(|c| matches!(c, Chunk::Delete(_))).count();
    let inserts: usize = chunks.iter().filter(|c| matches!(c, Chunk::Insert(_))).count();
    assert!(deletes > 0 || inserts > 0);
}

#[test]
fn chunks_multiline() {
    let old = "line1\nline2\nline3\n";
    let new = "line1\nline modified\nline3\n";
    let chunks = compute_diff_chunks(old, new);

    // Should have at least some Equal chunks for the unchanged lines
    let has_equal = chunks.iter().any(|c| matches!(c, Chunk::Equal(_)));
    assert!(has_equal);

    // Should have a delete and insert for the changed line
    let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
    let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
    assert!(has_delete || has_insert);
}

#[test]
fn chunks_unicode() {
    let old = "hello \u{00e9}l\u{00e8}ve";
    let new = "hello \u{00e9}l\u{00e8}ve you";
    let chunks = compute_diff_chunks(old, new);

    // Common prefix should include unicode chars
    let prefix = match &chunks[0] {
        Chunk::Equal(s) => s,
        _ => panic!("expected Equal prefix"),
    };
    assert!(prefix.starts_with("hello "));
}

#[test]
fn chunks_append_only() {
    let old = "a\nb\n";
    let new = "a\nb\nc\nd\n";
    let chunks = compute_diff_chunks(old, new);
    let has_insert = chunks.iter().any(|c| matches!(c, Chunk::Insert(_)));
    assert!(has_insert);
}

#[test]
fn chunks_remove_only() {
    let old = "a\nb\nc\n";
    let new = "a\n";
    let chunks = compute_diff_chunks(old, new);
    let has_delete = chunks.iter().any(|c| matches!(c, Chunk::Delete(_)));
    assert!(has_delete);
}
