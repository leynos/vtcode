//! Usage-accounting regression tests for OpenAI-compatible payloads.
//!
//! Chat, Responses and cache fixtures live together here so accounting
//! changes have one place to update.

use super::parse_usage_openai_format;
use serde_json::json;

#[test]
fn parse_usage_openai_format_extracts_basic_fields() {
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150
        }
    });
    let usage = parse_usage_openai_format(&response, false).expect("usage expected");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.total_tokens, 150);
    assert_eq!(usage.reasoning_output_tokens, None);
    assert_eq!(usage.cached_prompt_tokens, None);
    assert_eq!(usage.cache_creation_tokens, None);
}

#[test]
fn parse_usage_openai_format_extracts_nested_reasoning_tokens() {
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "completion_tokens_details": {
                "reasoning_tokens": 35
            }
        }
    });

    let usage = parse_usage_openai_format(&response, false).expect("usage expected");
    assert_eq!(usage.reasoning_output_tokens, Some(35));
}

#[test]
fn parse_usage_openai_format_accepts_responses_reasoning_details() {
    let response = json!({
        "usage": {
            "input_tokens": 100,
            "output_tokens": 50,
            "total_tokens": 150,
            "output_tokens_details": {
                "reasoning_tokens": 35
            }
        }
    });

    let usage = parse_usage_openai_format(&response, false).expect("usage expected");
    // Responses names the base counts `input_tokens`/`output_tokens`; reading
    // only the Chat names would zero both and clamp the reasoning count.
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.reasoning_output_tokens, Some(35));
    assert_eq!(usage.total_tokens, 150);
}

#[test]
fn parse_usage_openai_format_rejects_counts_that_do_not_fit_u32() {
    let response = json!({
        "usage": {
            "prompt_tokens": 4_294_967_296u64,
            "completion_tokens": 5,
            "total_tokens": 4_294_967_301u64
        }
    });

    assert!(
        parse_usage_openai_format(&response, false).is_none(),
        "a truncated conversion must not publish a wrapped count"
    );
}

#[test]
fn parse_usage_openai_format_includes_cache_metrics_when_enabled() {
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_cache_hit_tokens": 30,
            "prompt_cache_miss_tokens": 70
        }
    });
    let usage = parse_usage_openai_format(&response, true).expect("usage expected");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.completion_tokens, 50);
    assert_eq!(usage.total_tokens, 150);
    assert_eq!(usage.cached_prompt_tokens, Some(30));
    // A miss is an uncached input token, not a priced cache write. Counting
    // it as creation charged it at the write rate while the prompt total
    // already carried it at the input rate.
    assert_eq!(usage.cache_creation_tokens, None);
}

#[test]
fn parse_usage_openai_format_reports_a_separately_priced_cache_write() {
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": {
                "cached_tokens": 30,
                "cache_write_tokens": 12
            }
        }
    });
    let usage = parse_usage_openai_format(&response, true).expect("usage expected");
    assert_eq!(usage.cached_prompt_tokens, Some(30));
    assert_eq!(usage.cache_creation_tokens, Some(12));
    assert_eq!(
        parse_usage_openai_format(&response, false)
            .expect("usage expected")
            .cache_creation_tokens,
        None,
        "cache metrics stay suppressed when the caller opts out"
    );
}

#[test]
fn parse_usage_openai_format_reads_nested_cached_tokens() {
    for usage_value in [
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_tokens_details": { "cached_tokens": 40 }
        }),
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "input_tokens_details": { "cached_tokens": 40 }
        }),
        json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "cached_tokens": 40
        }),
    ] {
        let response = json!({ "usage": usage_value });
        let usage = parse_usage_openai_format(&response, true).expect("usage expected");
        assert_eq!(usage.cached_prompt_tokens, Some(40), "usage {usage:?}");
        assert_eq!(
            parse_usage_openai_format(&response, false)
                .expect("usage expected")
                .cached_prompt_tokens,
            None
        );
    }
}

#[test]
fn parse_usage_openai_format_excludes_cache_metrics_when_disabled() {
    let response = json!({
        "usage": {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
            "prompt_cache_hit_tokens": 30,
            "prompt_cache_miss_tokens": 70
        }
    });
    let usage = parse_usage_openai_format(&response, false).expect("usage expected");
    assert_eq!(usage.prompt_tokens, 100);
    assert_eq!(usage.cached_prompt_tokens, None);
    assert_eq!(usage.cache_creation_tokens, None);
}

#[test]
fn parse_usage_openai_format_handles_missing_usage() {
    let response = json!({"choices": []});
    assert!(parse_usage_openai_format(&response, true).is_none());
}

#[test]
fn parse_usage_openai_format_rejects_unpopulated_usage() {
    let null_usage = json!({"choices": [], "usage": null});
    let empty_usage = json!({"choices": [], "usage": {}});

    assert!(parse_usage_openai_format(&null_usage, true).is_none());
    assert!(parse_usage_openai_format(&empty_usage, true).is_none());
}

#[test]
fn parse_usage_openai_format_keeps_explicitly_reported_zeros() {
    let response = json!({
        "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
    });
    let usage = parse_usage_openai_format(&response, true).expect("reported zero usage");

    assert_eq!(usage.total_tokens, 0);
}
