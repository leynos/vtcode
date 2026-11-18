//! Covers the rate-limit notice body built from a provider error.
//!
//! A notice may describe only what the provider supplied and what Diapason
//! actually scheduled, so these tests pin the status, the `Retry-After`
//! echo, the retry claim, and the errors that publish nothing.

use super::*;
use diapason_core::llm::provider::LLMErrorMetadata;

/// Assert the notice envelope carries the expected update kind, level, and source.
fn assert_notice_envelope(value: &Value, level: &str, source: &str) {
    assert_eq!(value["sessionUpdate"], "session_info_update");
    assert_eq!(value["_meta"]["lody"]["notice"]["level"], level);
    assert_eq!(value["_meta"]["lody"]["notice"]["source"], source);
}

/// Assert every fragment appears in `message`.
fn assert_contains_all(message: &str, fragments: &[&str]) {
    for fragment in fragments {
        assert!(message.contains(fragment), "expected {fragment:?} in {message:?}");
    }
}

/// Assert retry metadata is reported without inventing a quota window.
#[test]
fn rate_limit_notice_preserves_retry_after_without_fabricating_a_window() {
    let error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "baseten",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("17".to_string()),
            None,
        )),
    };
    let update = rate_limit_notice_update("baseten", &error, Some(Duration::from_secs(20))).expect("rate-limit notice");
    let value = serde_json::to_value(update).expect("notice JSON");

    assert_notice_envelope(&value, "warning", "provider_rate_limit");
    let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");
    assert_contains_all(message, &["HTTP 429", "Retry-After: 17", "retry in 20.0s"]);
    assert!(value["_meta"]["lody"].get("rateLimits").is_none());
}

/// Assert an unsafe provider `Retry-After` hint is omitted from the notice.
#[test]
fn unsafe_retry_after_hint_is_omitted() {
    let cases = [
        ("x".repeat(RETRY_AFTER_MAX_BYTES + 1), "over-long hint"),
        ("17\r\nforged: yes".to_string(), "control characters"),
    ];

    for (hint, label) in cases {
        let error = LLMError::RateLimit {
            metadata: Some(LLMErrorMetadata::new(
                "baseten",
                Some(429),
                Some("rate_limit_error".to_string()),
                None,
                None,
                Some(hint.clone()),
                None,
            )),
        };
        let update = rate_limit_notice_update("baseten", &error, None).expect("rate-limit notice");
        let value = serde_json::to_value(update).expect("notice JSON");
        let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");

        assert!(!message.contains("Retry-After"), "{label} must be omitted: {message}");
        assert!(!message.contains(&hint), "{label} must not be echoed: {message}");
        assert!(message.contains("no further automatic retry is scheduled"));
    }
}

/// Assert a rate limit without metadata still publishes a notice.
#[test]
fn rate_limit_without_metadata_still_publishes_a_notice() {
    let error = LLMError::RateLimit { metadata: None };
    let update = rate_limit_notice_update("baseten", &error, None).expect("generic rate-limit notice");
    let value = serde_json::to_value(update).expect("notice JSON");
    let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");

    assert_notice_envelope(&value, "warning", "provider_rate_limit");
    assert_contains_all(message, &["rate limit", "no further automatic retry is scheduled"]);
    assert!(
        !message.contains("HTTP") && !message.contains("Retry-After"),
        "a missing status must not be reported as HTTP 429 or Retry-After: {message}"
    );
}

/// Assert a missing HTTP status is not reported as a 429.
#[test]
fn rate_limit_without_status_does_not_claim_http_429() {
    let error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "baseten",
            None,
            Some("rate_limit_error".to_string()),
            None,
            None,
            None,
            None,
        )),
    };
    let update = rate_limit_notice_update("baseten", &error, None).expect("generic rate-limit notice");
    let value = serde_json::to_value(update).expect("notice JSON");
    let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");

    assert!(!message.contains("HTTP 429"), "an unstated status must not be named: {message}");
    assert!(!message.contains("Retry-After"));
}

/// Assert non-rate-limit errors publish no notice.
#[test]
fn non_rate_limit_errors_do_not_emit_rate_limit_notices() {
    let error = LLMError::Network {
        message: "connection reset".to_string(),
        metadata: None,
    };
    assert!(rate_limit_notice_update("baseten", &error, None).is_none());
}

fn notice_message(limits: Option<RateLimitMetadata>) -> String {
    let mut metadata = LLMErrorMetadata::new("fixture", Some(429), None, None, None, None, None);
    metadata.rate_limit = limits;
    let error = LLMError::RateLimit { metadata: Some(metadata) };
    let value = serde_json::to_value(rate_limit_notice_update("fixture", &error, Some(Duration::from_secs(10))))
        .expect("serialize notice");
    value["_meta"]["lody"]["notice"]["message"]
        .as_str()
        .expect("notice message")
        .to_owned()
}

#[test]
fn baseten_notice_includes_zero_remaining_and_preserves_minute_units() {
    let message = notice_message(Some(RateLimitMetadata {
        requests_limit_per_minute: Some(60),
        requests_remaining_per_minute: Some(0),
        tokens_limit_per_minute: Some(100_000),
        tokens_remaining_per_minute: Some(1_000),
        ..Default::default()
    }));
    assert_contains_all(
        &message,
        &[
            "request limit/min: 60",
            "requests remaining/min: 0",
            "token limit/min: 100000",
            "tokens remaining/min: 1000",
        ],
    );
    assert!(!message.contains("limit/s:"));
}

#[test]
fn fireworks_notice_distinguishes_limits_from_request_usage() {
    let message = notice_message(Some(RateLimitMetadata {
        prompt_tokens_limit_per_second: Some(500),
        cache_adjusted_prompt_tokens_limit_per_second: Some(250),
        generated_tokens_limit_per_second: Some(50),
        prompt_tokens: Some(101),
        cached_prompt_tokens: Some(40),
        ..Default::default()
    }));
    assert_contains_all(
        &message,
        &[
            "prompt token limit/s: 500",
            "cache-adjusted prompt token limit/s: 250",
            "generated token limit/s: 50",
            "request prompt tokens: 101",
            "request cached prompt tokens: 40",
        ],
    );
    assert!(!message.contains("/min:"));
}

#[test]
fn together_notice_reports_fractional_reset_interval() {
    let message = notice_message(Some(RateLimitMetadata {
        reset_after_millis: Some(1_250),
        ..Default::default()
    }));
    assert!(message.contains("provider reset interval: 1.250s"));
}

#[test]
fn missing_headers_leave_the_ordinary_429_notice() {
    assert_eq!(notice_message(None), "fixture returned HTTP 429 (rate limited); Diapason will retry in 10.0s");
    assert_eq!(notice_message(Some(RateLimitMetadata::default())), notice_message(None));
    assert!(rate_limit_notice_update("fixture", &LLMError::RateLimit { metadata: None }, None).is_some());
}

/// Assert a metadata status of 429 is reported even for an unclassified error.
#[test]
fn metadata_status_429_is_reported_even_when_error_is_not_reclassified() {
    let error = LLMError::Provider {
        message: "provider capacity exhausted".to_string(),
        metadata: Some(LLMErrorMetadata::new(
            "baseten",
            Some(429),
            Some("capacity_exhausted".to_string()),
            None,
            None,
            Some("10".to_string()),
            None,
        )),
    };

    assert!(rate_limit_notice_update("baseten", &error, None).is_some());
}
