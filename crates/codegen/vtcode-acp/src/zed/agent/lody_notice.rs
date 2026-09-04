use crate::acp;
use serde_json::{Map, Value};
use std::time::Duration;
use tracing::warn;
use vtcode_commons::llm::RateLimitMetadata;
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

use super::ZedAgent;

impl ZedAgent {
    pub(super) async fn publish_rate_limit_notice(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        error: &LLMError,
        retry_delay: Option<Duration>,
    ) {
        if is_rate_limit(error) {
            if let Some(limits) = rate_limit_metadata(error).and_then(|metadata| metadata.rate_limit.as_ref()) {
                self.publish_lody_rate_limits(session_id, provider, limits);
            }
        }
        let Some(update) = rate_limit_notice_update(provider, error, retry_delay) else {
            return;
        };
        if let Err(error) = self.send_update(session_id, update).await {
            warn!(%error, %session_id, "Failed to publish ACP provider rate-limit notice");
        }
    }
}

fn rate_limit_notice_update(
    provider: &str,
    error: &LLMError,
    retry_delay: Option<Duration>,
) -> Option<acp::SessionUpdate> {
    if !is_rate_limit(error) {
        return None;
    }
    let metadata = rate_limit_metadata(error);
    let retry_after = metadata.and_then(|metadata| metadata.retry_after.as_deref());
    let mut message = format!("{provider} returned HTTP 429 (rate limited)");
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

fn is_rate_limit(error: &LLMError) -> bool {
    matches!(error, LLMError::RateLimit { .. })
        || rate_limit_metadata(error).is_some_and(|metadata| metadata.status == Some(429))
}

fn rate_limit_metadata(error: &LLMError) -> Option<&LLMErrorMetadata> {
    match error {
        LLMError::Authentication { metadata, .. }
        | LLMError::RateLimit { metadata }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => metadata.as_deref(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vtcode_core::llm::provider::LLMErrorMetadata;

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

        assert_eq!(value["sessionUpdate"], "session_info_update");
        assert_eq!(value["_meta"]["lody"]["notice"]["level"], "warning");
        assert_eq!(value["_meta"]["lody"]["notice"]["source"], "provider_rate_limit");
        let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");
        assert!(message.contains("Retry-After: 17"));
        assert!(message.contains("retry in 20.0s"));
        assert!(value["_meta"]["lody"].get("rateLimits").is_none());
    }

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
        assert!(message.contains("request limit/min: 60"));
        assert!(message.contains("requests remaining/min: 0"));
        assert!(message.contains("token limit/min: 100000"));
        assert!(message.contains("tokens remaining/min: 1000"));
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
        assert!(message.contains("prompt token limit/s: 500"));
        assert!(message.contains("cache-adjusted prompt token limit/s: 250"));
        assert!(message.contains("generated token limit/s: 50"));
        assert!(message.contains("request prompt tokens: 101"));
        assert!(message.contains("request cached prompt tokens: 40"));
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
