//! Extracted regression tests; production source remains byte-identical.

use super::Usage;

#[test]
fn cache_helpers_fall_back_to_cached_prompt_tokens() {
    let usage = Usage {
        prompt_tokens: 1_000,
        completion_tokens: 200,
        total_tokens: 1_200,
        cached_prompt_tokens: Some(600),
        cache_creation_tokens: Some(150),
        cache_read_tokens: None,
        iterations: None,
    };

    assert_eq!(usage.cache_read_tokens_or_fallback(), 600);
    assert_eq!(usage.cache_creation_tokens_or_zero(), 150);
    assert_eq!(usage.total_cache_tokens(), 750);
    assert_eq!(usage.is_cache_hit(), Some(true));
    assert_eq!(usage.is_cache_miss(), Some(false));
    assert_eq!(usage.cache_savings_ratio(), Some(0.6));
    assert_eq!(usage.cache_hit_rate(), Some(80.0));
}

#[test]
fn cache_helpers_preserve_unknown_without_metrics() {
    let usage = Usage {
        prompt_tokens: 1_000,
        completion_tokens: 200,
        total_tokens: 1_200,
        cached_prompt_tokens: None,
        cache_creation_tokens: None,
        cache_read_tokens: None,
        iterations: None,
    };

    assert_eq!(usage.total_cache_tokens(), 0);
    assert_eq!(usage.is_cache_hit(), None);
    assert_eq!(usage.is_cache_miss(), None);
    assert_eq!(usage.cache_savings_ratio(), None);
    assert_eq!(usage.cache_hit_rate(), None);
}
