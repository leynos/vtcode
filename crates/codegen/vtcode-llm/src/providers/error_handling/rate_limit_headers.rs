use super::ApiResponseMetadata;
use crate::providers::common::extract_header;
use reqwest::header::{HeaderMap, HeaderName};
use vtcode_commons::llm::RateLimitMetadata;
use vtcode_config::core::RateLimitHeaderConfig;

const MAX_RESET_AFTER_MILLIS: u64 = 86_400_000;

pub(super) fn extract_response_metadata(
    headers: &HeaderMap,
    rate_limit_headers: &RateLimitHeaderConfig,
) -> ApiResponseMetadata {
    ApiResponseMetadata {
        request_id: extract_header(headers, &["request-id", "x-request-id", "openai-request-id"]),
        organization_id: extract_header(
            headers,
            &["anthropic-organization-id", "openai-organization", "x-organization-id"],
        ),
        retry_after: extract_header(headers, &["retry-after"]),
        rate_limit: extract_rate_limit_metadata(headers, rate_limit_headers),
    }
}

fn extract_rate_limit_metadata(
    headers: &HeaderMap,
    header_config: &RateLimitHeaderConfig,
) -> Option<RateLimitMetadata> {
    let metadata = RateLimitMetadata {
        requests_limit_per_minute: extract_u64_header(headers, &header_config.requests_limit_per_minute),
        requests_remaining_per_minute: extract_u64_header(headers, &header_config.requests_remaining_per_minute),
        tokens_limit_per_minute: extract_u64_header(headers, &header_config.tokens_limit_per_minute),
        tokens_remaining_per_minute: extract_u64_header(headers, &header_config.tokens_remaining_per_minute),
        requests_limit_per_second: extract_u64_header(headers, &header_config.requests_limit_per_second),
        requests_remaining_per_second: extract_u64_header(headers, &header_config.requests_remaining_per_second),
        tokens_limit_per_second: extract_u64_header(headers, &header_config.tokens_limit_per_second),
        tokens_remaining_per_second: extract_u64_header(headers, &header_config.tokens_remaining_per_second),
        prompt_tokens_limit_per_second: extract_u64_header(headers, &header_config.prompt_tokens_limit_per_second),
        cache_adjusted_prompt_tokens_limit_per_second: extract_u64_header(
            headers,
            &header_config.cache_adjusted_prompt_tokens_limit_per_second,
        ),
        generated_tokens_limit_per_second: extract_u64_header(
            headers,
            &header_config.generated_tokens_limit_per_second,
        ),
        prompt_tokens: extract_u64_header(headers, &header_config.prompt_tokens),
        cached_prompt_tokens: extract_u64_header(headers, &header_config.cached_prompt_tokens),
        reset_after_millis: extract_reset_after_millis(headers, &header_config.reset_after_seconds),
    };

    (!metadata.is_empty()).then_some(metadata)
}

fn extract_u64_header(headers: &HeaderMap, configured_name: &Option<String>) -> Option<u64> {
    let name = HeaderName::from_bytes(configured_name.as_deref()?.as_bytes()).ok()?;
    parse_ascii_u64(headers.get(name)?.to_str().ok()?.trim())
}

fn extract_reset_after_millis(headers: &HeaderMap, configured_name: &Option<String>) -> Option<u64> {
    let name = HeaderName::from_bytes(configured_name.as_deref()?.as_bytes()).ok()?;
    parse_reset_after_millis(headers.get(name)?.to_str().ok()?)
}

fn parse_reset_after_millis(raw_seconds: &str) -> Option<u64> {
    let raw_seconds = raw_seconds.trim();
    let (whole_seconds, fractional_seconds) = match raw_seconds.split_once('.') {
        Some((whole, fraction)) if !whole.is_empty() && !fraction.is_empty() && !fraction.contains('.') => {
            (whole, Some(fraction))
        }
        Some(_) => return None,
        None => (raw_seconds, None),
    };
    let whole_millis = parse_ascii_u64(whole_seconds)?.checked_mul(1_000)?;
    let fractional_millis = fractional_seconds.map_or(Some(0), parse_fractional_millis)?;
    let millis = whole_millis.checked_add(fractional_millis)?;
    (millis <= MAX_RESET_AFTER_MILLIS).then_some(millis)
}

fn parse_ascii_u64(value: &str) -> Option<u64> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn parse_fractional_millis(fraction: &str) -> Option<u64> {
    if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let milliseconds_digits = fraction.len().min(3);
    let mut milliseconds = parse_ascii_u64(&fraction[..milliseconds_digits])?;
    milliseconds *= 10_u64.pow(u32::try_from(3 - milliseconds_digits).ok()?);
    if fraction
        .as_bytes()
        .get(3..)
        .is_some_and(|tail| tail.iter().any(|digit| *digit != b'0'))
    {
        milliseconds += 1;
    }
    Some(milliseconds)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn malformed_or_overflowing_rate_limit_headers_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "unlimited".parse().expect("static header"));
        headers.insert("x-ratelimit-remaining-requests", "+12".parse().expect("signed header"));
        headers.insert("x-ratelimit-limit-tokens", "18446744073709551616".parse().expect("overflowing numeric header"));

        assert_eq!(extract_rate_limit_metadata(&headers, &RateLimitHeaderConfig::default()), None);
    }

    #[test]
    fn signed_reset_after_seconds_are_rejected() {
        assert_eq!(parse_reset_after_millis("+5"), None);
        assert_eq!(parse_reset_after_millis("5"), Some(5_000));
    }

    #[test]
    fn fireworks_limits_and_request_counters_keep_distinct_semantics() {
        let config = RateLimitHeaderConfig::for_provider_name("fireworks-proxy");
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-tokens-prompt", "60000".parse().expect("numeric header"));
        headers.insert("x-ratelimit-limit-tokens-cache-adjusted-prompt", "15000".parse().expect("numeric header"));
        headers.insert("x-ratelimit-limit-tokens-generated", "600".parse().expect("numeric header"));
        headers.insert("fireworks-prompt-tokens", "8000".parse().expect("numeric header"));
        headers.insert("fireworks-cached-prompt-tokens", "7500".parse().expect("numeric header"));

        let metadata = extract_rate_limit_metadata(&headers, &config).expect("Fireworks metadata");
        assert_eq!(metadata.prompt_tokens_limit_per_second, Some(60_000));
        assert_eq!(metadata.cache_adjusted_prompt_tokens_limit_per_second, Some(15_000));
        assert_eq!(metadata.generated_tokens_limit_per_second, Some(600));
        assert_eq!(metadata.prompt_tokens, Some(8_000));
        assert_eq!(metadata.cached_prompt_tokens, Some(7_500));
    }

    #[test]
    fn together_fractional_reset_rounds_up_without_overwriting_retry_after() {
        let config = RateLimitHeaderConfig::for_provider_name("Together");
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "2".parse().expect("static retry-after"));
        headers.insert("x-ratelimit-reset", "0.0001".parse().expect("fractional reset"));
        headers.insert("x-tokenlimit-limit", "2000".parse().expect("numeric header"));

        let metadata = extract_response_metadata(&headers, &config);
        assert_eq!(metadata.retry_after.as_deref(), Some("2"));
        let rate_limit = metadata.rate_limit.expect("Together metadata");
        assert_eq!(rate_limit.tokens_limit_per_second, Some(2_000));
        assert_eq!(rate_limit.reset_after_millis, Some(1));
    }

    proptest! {
        #[test]
        fn fractional_reset_never_rounds_below_wire_value(
            whole_seconds in 0_u64..86_400,
            fractional_millionths in 0_u32..1_000_000,
        ) {
            let wire_value = format!("{whole_seconds}.{fractional_millionths:06}");
            let parsed_millis = parse_reset_after_millis(&wire_value).expect("generated reset is in range");
            let exact_micros = whole_seconds * 1_000_000 + u64::from(fractional_millionths);

            prop_assert!(parsed_millis * 1_000 >= exact_micros);
            prop_assert!(parsed_millis * 1_000 < exact_micros + 1_000);
        }
    }
}
