//! Cache-token parsing for OpenAI-compatible usage payloads.
//!
//! Providers report cache counts under several shapes, and may send an
//! explicit `null` for one shape while reporting a count in another. Both
//! parsers therefore take the first candidate that fits `u32`, so an unusable
//! value never masks a later reported count.

use serde_json::Value;

pub(crate) fn parse_cached_prompt_tokens_from_usage(
    usage_value: &Value,
    include_cached_prompt_metrics: bool,
) -> Option<u32> {
    if !include_cached_prompt_metrics {
        return None;
    }

    // Select the first candidate that converts to `u32`. Providers can send an
    // explicit `null` for one shape while reporting a count in another, and a
    // value that is absent, null, non-numeric or too large must not mask a
    // later usable count.
    let cached_prompt_tokens = [
        usage_value
            .get("input_tokens_details")
            .and_then(|details| details.get("cached_tokens")),
        usage_value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cached_tokens")),
        usage_value.get("prompt_cache_hit_tokens"),
        usage_value.get("cached_tokens"),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_u64)
    .find_map(|value| u32::try_from(value).ok());

    if let Some(cached_prompt_tokens) = cached_prompt_tokens {
        tracing::debug!(
            target = "diapason::llm::responses::prompt_cache",
            cached_prompt_tokens,
            "Responses cached prompt token usage"
        );
    }

    cached_prompt_tokens
}

/// Parse OpenAI `cache_write_tokens` from a Responses/Chat usage payload.
///
/// GPT-5.6 and later bill cache writes at 1.25x the input rate; downstream
/// cost math consumes this via `Usage::cache_creation_tokens`.
/// Mirrors [`parse_cached_prompt_tokens_from_usage`], including the metrics
/// gate: no value is reported when cached-prompt metrics are disabled.
pub(crate) fn parse_cache_write_tokens_from_usage(
    usage_value: &Value,
    include_cached_prompt_metrics: bool,
) -> Option<u32> {
    if !include_cached_prompt_metrics {
        return None;
    }

    // Same rule as [`parse_cached_prompt_tokens_from_usage`]: the first
    // candidate that converts to `u32` wins, so an unusable shape never masks
    // a later reported count.
    let cache_write_tokens = [
        usage_value
            .get("input_tokens_details")
            .and_then(|details| details.get("cache_write_tokens")),
        usage_value
            .get("prompt_tokens_details")
            .and_then(|details| details.get("cache_write_tokens")),
    ]
    .into_iter()
    .flatten()
    .filter_map(Value::as_u64)
    .find_map(|value| u32::try_from(value).ok());

    if let Some(cache_write_tokens) = cache_write_tokens {
        tracing::debug!(
            target = "diapason::llm::responses::prompt_cache",
            cache_write_tokens,
            "Responses cache-write token usage"
        );
    }

    cache_write_tokens
}

#[cfg(test)]
mod tests;
