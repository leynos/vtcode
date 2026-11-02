//! Publishes provider rate-limit warnings as ACP session-info notices carrying
//! `_meta.lody.notice`, reporting only retry information the provider actually
//! supplied and never a fabricated quota window.

use crate::acp;
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;
use vtcode_commons::{ErrorCategory, llm::RateLimitMetadata};
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

use super::{
    ZedAgent,
    provider_telemetry::{ProviderMetricContext, ProviderMetricObservation, ProviderMetricSink},
};
use crate::zed::provider_runtime::ProviderClass;

const NOTICE_OPERATION: &str = "rate_limit_notice";

/// Request-local inputs for a bounded ACP rate-limit notification.
pub(super) struct RateLimitNotice<'a> {
    pub(super) session_id: &'a acp::SessionId,
    pub(super) provider: &'a str,
    pub(super) error: &'a LLMError,
    pub(super) retry_delay: Option<Duration>,
    pub(super) observed_at: SystemTime,
    pub(super) metric_sink: &'a ProviderMetricSink,
    pub(super) provider_class: ProviderClass,
}

/// A rate-limit observation published outside a provider turn's telemetry context.
pub(super) struct ObservedRateLimitNotice<'a> {
    pub(super) session_id: &'a acp::SessionId,
    pub(super) provider: &'a str,
    pub(super) error: &'a LLMError,
    pub(super) retry_delay: Option<Duration>,
    pub(super) observed_at: SystemTime,
}

impl ZedAgent {
    /// Publish a warning notice for an observation made outside a provider
    /// turn's telemetry context.
    ///
    /// `retry_delay` is the delay VTCode actually scheduled, or `None` when no
    /// further attempt will be made. Errors that report no rate limit publish
    /// nothing.
    pub(super) async fn publish_rate_limit_notice_at(&self, notice: ObservedRateLimitNotice<'_>) {
        let ObservedRateLimitNotice {
            session_id,
            provider,
            error,
            retry_delay,
            observed_at,
        } = notice;
        self.publish_rate_limit_notice_with_context(RateLimitNotice {
            session_id,
            provider,
            error,
            retry_delay,
            observed_at,
            metric_sink: &ProviderMetricSink::production(),
            provider_class: ProviderClass::Unknown,
        })
        .await;
    }

    pub(super) async fn publish_rate_limit_notice_with_context(&self, notice: RateLimitNotice<'_>) {
        let RateLimitNotice {
            session_id,
            provider,
            error,
            retry_delay,
            observed_at,
            metric_sink,
            provider_class,
        } = notice;
        let observed_epoch_seconds = observed_at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
        if let Some(limits) = rate_limit_metadata(error)
            .flatten()
            .and_then(|metadata| metadata.rate_limit.as_ref())
        {
            self.publish_lody_rate_limits_at(super::lody_rate_limits::LodyRateLimitObservation {
                session_id,
                provider,
                limits,
                observed_epoch_seconds,
                metric_sink,
                provider_class,
            })
            .await;
        }
        let Some(update) = rate_limit_notice_update(provider, error, retry_delay) else {
            return;
        };
        if self.send_update(session_id, update).await.is_err() {
            metric_sink
                .record(ProviderMetricObservation::RateLimitNotificationFailure(
                    ProviderMetricContext::rate_limit_notification_failure(provider_class, NOTICE_OPERATION),
                ))
                .await;
            warn!(
                error_category = ErrorCategory::ExecutionError.as_str(),
                "Failed to publish ACP provider rate-limit notice"
            );
        }
    }
}

/// Build the session-info update for a rate-limit error, if it is one.
fn rate_limit_notice_update(
    provider: &str,
    error: &LLMError,
    retry_delay: Option<Duration>,
) -> Option<acp::SessionUpdate> {
    let metadata = rate_limit_metadata(error)?;
    let retry_after = metadata.and_then(|metadata| metadata.retry_after.as_deref());
    let mut message = match metadata.and_then(|metadata| metadata.status) {
        Some(status) => format!("{provider} returned HTTP {status} (rate limited)"),
        None => format!("{provider} reported a rate limit"),
    };
    if let Some(retry_after) = retry_after {
        message.push_str(&format!("; provider Retry-After: {retry_after}"));
    }
    if let Some(limits) = metadata.and_then(|metadata| metadata.rate_limit.as_ref()) {
        append_rate_limit_details(&mut message, limits);
    }
    if let Some(delay) = retry_delay {
        message.push_str(&format!("; VTCode will retry in {:.1}s", delay.as_secs_f64()));
    } else {
        message.push_str("; no further automatic retry is scheduled");
    }

    let mut notice = Map::new();
    let _ = notice.insert("level".to_string(), Value::String("warning".to_string()));
    let _ = notice.insert("message".to_string(), Value::String(message));
    let _ = notice.insert("source".to_string(), Value::String("provider_rate_limit".to_string()));
    let mut lody = Map::new();
    let _ = lody.insert("notice".to_string(), Value::Object(notice));
    let mut meta = Map::new();
    let _ = meta.insert("lody".to_string(), Value::Object(lody));
    Some(acp::SessionUpdate::SessionInfoUpdate(acp::SessionInfoUpdate::new().meta(meta)))
}

fn append_rate_limit_details(message: &mut String, limits: &RateLimitMetadata) {
    for (label, value) in [
        ("request limit/min", limits.requests_limit_per_minute),
        ("requests remaining/min", limits.requests_remaining_per_minute),
        ("token limit/min", limits.tokens_limit_per_minute),
        ("tokens remaining/min", limits.tokens_remaining_per_minute),
        ("request limit/s", limits.requests_limit_per_second),
        ("requests remaining/s", limits.requests_remaining_per_second),
        ("token limit/s", limits.tokens_limit_per_second),
        ("tokens remaining/s", limits.tokens_remaining_per_second),
        ("prompt token limit/s", limits.prompt_tokens_limit_per_second),
        ("cache-adjusted prompt token limit/s", limits.cache_adjusted_prompt_tokens_limit_per_second),
        ("generated token limit/s", limits.generated_tokens_limit_per_second),
        ("request prompt tokens", limits.prompt_tokens),
        ("request cached prompt tokens", limits.cached_prompt_tokens),
    ] {
        if let Some(value) = value {
            message.push_str(&format!("; {label}: {value}"));
        }
    }
    if let Some(millis) = limits.reset_after_millis {
        message.push_str(&format!("; provider reset interval: {:.3}s", Duration::from_millis(millis).as_secs_f64()));
    }
}

/// Classify `error` as a rate limit.
///
/// The outer option says whether the error is a rate limit at all; the inner
/// option carries the provider metadata, which may be absent even for
/// [`LLMError::RateLimit`]. A notice is still published in that case, without
/// claiming an HTTP status the provider never supplied.
fn rate_limit_metadata(error: &LLMError) -> Option<Option<&LLMErrorMetadata>> {
    let (is_rate_limit, metadata) = match error {
        LLMError::RateLimit { metadata } => (true, metadata.as_deref()),
        LLMError::Authentication { metadata, .. }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => {
            let metadata = metadata.as_deref()?;
            (metadata.status == Some(429), Some(metadata))
        }
    };
    is_rate_limit.then_some(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_core::llm::provider::LLMErrorMetadata;

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
        let update =
            rate_limit_notice_update("baseten", &error, Some(Duration::from_secs(20))).expect("rate-limit notice");
        let value = serde_json::to_value(update).expect("notice JSON");

        assert_notice_envelope(&value, "warning", "provider_rate_limit");
        let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");
        assert_contains_all(message, &["HTTP 429", "Retry-After: 17", "retry in 20.0s"]);
        assert!(value["_meta"]["lody"].get("rateLimits").is_none());
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
        assert_eq!(notice_message(None), "fixture returned HTTP 429 (rate limited); VTCode will retry in 10.0s");
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
}
