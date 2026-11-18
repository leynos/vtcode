//! Precedence tests for the cache-token parsers.
//!
//! The property cases compare the parsers with an independent model of the
//! documented precedence rule, so a parser that stops at an unusable value or
//! reorders the shapes fails instead of agreeing with itself.

use super::*;
use proptest::prelude::*;
use serde_json::{Map, json};

#[test]
fn nested_null_cached_tokens_falls_back_to_a_reported_count() {
    let usage = json!({
        "prompt_tokens_details": {"cached_tokens": null},
        "prompt_cache_hit_tokens": 42
    });

    assert_eq!(parse_cached_prompt_tokens_from_usage(&usage, true), Some(42));
}

#[test]
fn nested_null_cache_write_tokens_falls_back_to_a_reported_count() {
    let usage = json!({
        "input_tokens_details": {"cache_write_tokens": null},
        "prompt_tokens_details": {"cache_write_tokens": 17}
    });

    assert_eq!(parse_cache_write_tokens_from_usage(&usage, true), Some(17));
}

/// Generate a value a provider might place in a cache-token field.
///
/// Covered shapes: absent (by wrapping in `option::of`), explicit `null`,
/// in-range counts, counts above `u32::MAX`, negative and fractional
/// numbers, text, and booleans. Only the in-range counts are usable, so a
/// later usable candidate must survive every unusable earlier shape.
fn candidate_token_value() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<u32>().prop_map(|count| json!(count)),
        any::<u64>().prop_map(|count| json!(count)),
        any::<i64>().prop_map(|count| json!(count)),
        (-1_000_000.0f64..1_000_000.0).prop_map(|count| json!(count)),
        "[a-z]{0,8}".prop_map(Value::String),
        Just(json!(true)),
    ]
}

/// The documented precedence: the first candidate that fits `u32`.
///
/// Computed here rather than by the production parser, so a regression
/// that stops the search at an unusable value, or reorders the shapes,
/// fails the property instead of agreeing with itself.
fn expected_token_count(candidates: &[Option<Value>], include: bool) -> Option<u32> {
    if !include {
        return None;
    }
    candidates
        .iter()
        .flatten()
        .find_map(|value| value.as_u64().and_then(|count| u32::try_from(count).ok()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Assert cached-prompt parsing follows the precedence rule for every
    /// mix of absent, null, invalid, oversized and usable candidates.
    #[test]
    fn cached_prompt_token_fallback_matches_an_independent_precedence_model(
        input_details in proptest::option::of(candidate_token_value()),
        prompt_details in proptest::option::of(candidate_token_value()),
        prompt_cache_hit in proptest::option::of(candidate_token_value()),
        top_level in proptest::option::of(candidate_token_value()),
        include in any::<bool>(),
    ) {
        let mut usage = Map::new();
        if let Some(value) = &input_details {
            usage.insert("input_tokens_details".to_string(), json!({"cached_tokens": value}));
        }
        if let Some(value) = &prompt_details {
            usage.insert("prompt_tokens_details".to_string(), json!({"cached_tokens": value}));
        }
        if let Some(value) = &prompt_cache_hit {
            usage.insert("prompt_cache_hit_tokens".to_string(), value.clone());
        }
        if let Some(value) = &top_level {
            usage.insert("cached_tokens".to_string(), value.clone());
        }
        let usage = Value::Object(usage);

        let candidates = [input_details, prompt_details, prompt_cache_hit, top_level];
        prop_assert_eq!(
            parse_cached_prompt_tokens_from_usage(&usage, include),
            expected_token_count(&candidates, include)
        );
    }

    /// Assert cache-write parsing follows the same precedence rule.
    #[test]
    fn cache_write_token_fallback_matches_an_independent_precedence_model(
        input_details in proptest::option::of(candidate_token_value()),
        prompt_details in proptest::option::of(candidate_token_value()),
        include in any::<bool>(),
    ) {
        let mut usage = Map::new();
        if let Some(value) = &input_details {
            usage.insert("input_tokens_details".to_string(), json!({"cache_write_tokens": value}));
        }
        if let Some(value) = &prompt_details {
            usage.insert("prompt_tokens_details".to_string(), json!({"cache_write_tokens": value}));
        }
        let usage = Value::Object(usage);

        let candidates = [input_details, prompt_details];
        prop_assert_eq!(
            parse_cache_write_tokens_from_usage(&usage, include),
            expected_token_count(&candidates, include)
        );
    }
}

#[test]
fn cache_metrics_are_absent_when_disabled() {
    let usage = json!({"prompt_tokens_details": {"cached_tokens": 9, "cache_write_tokens": 17}});

    assert_eq!(parse_cached_prompt_tokens_from_usage(&usage, false), None);
    assert_eq!(parse_cache_write_tokens_from_usage(&usage, false), None);
}
