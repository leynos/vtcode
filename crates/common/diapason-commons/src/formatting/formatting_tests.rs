//! Extracted regression tests; production source remains byte-identical.

use super::*;

#[test]
fn truncate_byte_budget_ascii() {
    assert_eq!(truncate_byte_budget("hello world", 5, "..."), "hello...");
    assert_eq!(truncate_byte_budget("hi", 10, "..."), "hi");
}

#[test]
fn truncate_byte_budget_cjk_no_panic() {
    // 'こ' = 3 bytes, 'ん' = 3 bytes → "こんにちは" = 15 bytes
    let jp = "こんにちは";
    // Cutting at 5 bytes lands inside 'ん' (bytes 3..6); must round down to 3.
    assert_eq!(truncate_byte_budget(jp, 5, "…"), "こ…");
    // Cutting at 6 lands on boundary
    assert_eq!(truncate_byte_budget(jp, 6, "…"), "こん…");
}

#[test]
fn truncate_byte_budget_mixed_ascii_cjk() {
    let mixed = "AB日本語CD";
    // A=1, B=1, 日=3, 本=3, 語=3, C=1, D=1 → 13 bytes total
    assert_eq!(truncate_byte_budget(mixed, 4, ".."), "AB.."); // mid-日 rounds to 2
    assert_eq!(truncate_byte_budget(mixed, 5, ".."), "AB日.."); // 2+3=5 exact
}

#[test]
fn truncate_byte_budget_emoji() {
    let emoji = "👋🌍"; // 4 bytes each = 8 bytes
    assert_eq!(truncate_byte_budget(emoji, 5, "!"), "👋!");
}

#[test]
fn truncate_byte_budget_zero() {
    assert_eq!(truncate_byte_budget("abc", 0, "..."), "...");
}

#[test]
fn compact_reasoning_text_collapses_blank_runs() {
    assert_eq!(compact_reasoning_text("line1\n\n\n\nline2\n"), "line1\n\nline2");
    assert_eq!(compact_reasoning_text("a\n\n\n\n\n\nb"), "a\n\nb");
}

#[test]
fn compact_reasoning_text_preserves_single_paragraph_breaks() {
    assert_eq!(compact_reasoning_text("para one\n\npara two\n"), "para one\n\npara two");
}

#[test]
fn compact_reasoning_text_trims_trailing_whitespace() {
    assert_eq!(compact_reasoning_text("  a  \n\n\n  b  \n"), "a\n\nb");
}

#[test]
fn compact_reasoning_text_strips_leading_trailing_blanks() {
    assert_eq!(compact_reasoning_text("\n\n\nmid\n\n\n"), "mid");
    assert_eq!(compact_reasoning_text("\n\n\n"), "");
    assert_eq!(compact_reasoning_text(""), "");
}

#[test]
fn wrap_text_words_basic_and_continuation_width() {
    assert_eq!(wrap_text_words("the quick brown fox", 9, 9), vec!["the quick", "brown fox"]);
    // First line wider than continuation lines.
    assert_eq!(wrap_text_words("alpha beta gamma delta", 11, 5), vec!["alpha beta", "gamma", "delta"]);
}

#[test]
fn wrap_text_words_blank_and_unicode() {
    assert!(wrap_text_words("   ", 5, 5).is_empty());
    // Must not panic on multi-byte chars and counts chars, not bytes.
    let wrapped = wrap_text_words("あいう えお かきく", 3, 3);
    assert_eq!(wrapped, vec!["あいう", "えお", "かきく"]);
}

#[test]
fn truncate_within_reserves_ellipsis_budget() {
    // Matches former runner::orchestration::truncate_chars behaviour.
    assert_eq!(truncate_within("hello world", 8, "..."), "hello...");
    assert_eq!(truncate_within("hi", 8, "..."), "hi");
    // Single-char ellipsis reserves exactly one char (former snapshots /
    // session_archive behaviour).
    assert_eq!(truncate_within("abcdef", 4, "…"), "abc…");
}

#[test]
fn truncate_within_counts_chars() {
    let jp = "あいうえお"; // 5 chars
    assert_eq!(truncate_within(jp, 5, "…"), jp);
    assert_eq!(truncate_within(jp, 3, "…"), "あい…");
}

#[test]
fn head_tail_truncate_keeps_both_ends() {
    let value = "0123456789".repeat(10); // 100 chars
    let (out, truncated) = head_tail_truncate(&value, 40, " ... [truncated] ... ");
    assert!(truncated);
    assert!(out.chars().count() <= 40);
    assert!(out.starts_with("012"));
    assert!(out.contains("[truncated]"));
    assert!(out.ends_with('9'));
}

#[test]
fn head_tail_truncate_passes_through_when_short() {
    let (out, truncated) = head_tail_truncate("short", 64, " ... ");
    assert_eq!(out, "short");
    assert!(!truncated);
}

#[test]
fn head_tail_truncate_small_budget_falls_back_to_prefix() {
    let marker = " ... [truncated] ... ";
    // max_chars <= marker_chars + 16 triggers the prefix fallback.
    // When max_chars (5) <= suffix_len (12), return just the prefix without suffix.
    let (out, truncated) = head_tail_truncate("abcdefghij", 5, marker);
    assert!(truncated);
    assert_eq!(out, "abcde");

    // When max_chars allows room for suffix, include it in the fallback branch.
    // Use max_chars=17 which is <= 21+16=37 (triggers fallback).
    let long_text = "abcdefghijklmnopqrstuvwxyz";
    let (out2, truncated2) = head_tail_truncate(long_text, 17, marker);
    assert!(truncated2);
    assert_eq!(out2, "abcde [truncated]");
    assert_eq!(out2.chars().count(), 17);
}

#[test]
fn truncate_text_counts_chars_not_bytes() {
    let jp = "あいうえお"; // 5 chars, 15 bytes
    assert_eq!(truncate_text(jp, 3, "…"), "あいう…");
    assert_eq!(truncate_text(jp, 5, "…"), "あいうえお");
}

#[test]
fn truncate_middle_keeps_both_ends() {
    assert_eq!(truncate_middle("short", 80), "short");
    assert_eq!(truncate_middle("abcdefghij", 5), "ab…ij");
    assert_eq!(truncate_middle("a b c", 80), "a b c");
    // Zero/one-char budgets.
    assert_eq!(truncate_middle("abc", 0), "");
    assert_eq!(truncate_middle("abc", 1), "…");
    // Control characters are sanitized to spaces before truncating.
    assert_eq!(truncate_middle("a\nb\tc", 80), "a b c");
}

#[test]
fn truncate_path_middle_breaks_at_separator() {
    assert_eq!(truncate_path_middle("src/lib.rs", 80), "src/lib.rs");
    assert_eq!(truncate_path_middle("foo/bar/baz/qux", 12), "foo…/qux");
    assert_eq!(truncate_path_middle("abc", 0), "");
}
