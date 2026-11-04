//! Compile-time coverage for the public stream-event contract.
//!
//! `LLMStreamEvent` is a public, non-`#[non_exhaustive]` enum. Adding a variant
//! is therefore a compile-time API change for downstream match expressions.
//! The pass fixtures compile an external consumer that constructs and matches
//! both tool-progress variants, so a rename, a removal, or a field-shape change
//! fails this harness rather than only a runtime test.

/// Compile an external consumer of both tool-progress stream-event variants.
#[test]
fn tool_progress_variants_are_constructible_and_matchable() {
    let tests = trybuild::TestCases::new();
    tests.pass("tests/ui/pass/*.rs");
}
