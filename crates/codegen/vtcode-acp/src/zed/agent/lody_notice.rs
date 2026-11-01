//! Publishes provider rate-limit warnings as ACP session-info notices carrying
//! `_meta.lody.notice`, reporting only retry information the provider actually
//! supplied and never a fabricated quota window.

use crate::acp;
use serde_json::{Map, Value};
use std::time::Duration;
use tracing::warn;
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

use super::ZedAgent;

impl ZedAgent {
    /// Publish a warning notice when `error` reports a provider rate limit.
    ///
    /// `retry_delay` is the delay VTCode actually scheduled, or `None` when no
    /// further attempt will be made. Errors that report no rate limit publish
    /// nothing.
    pub(super) async fn publish_rate_limit_notice(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        error: &LLMError,
        retry_delay: Option<Duration>,
    ) {
        let Some(update) = rate_limit_notice_update(provider, error, retry_delay) else {
            return;
        };
        if let Err(error) = self.send_update(session_id, update).await {
            warn!(%error, %session_id, "Failed to publish ACP provider rate-limit notice");
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
        assert!(message.contains("HTTP 429"));
        assert!(message.contains("Retry-After: 17"));
        assert!(message.contains("retry in 20.0s"));
        assert!(value["_meta"]["lody"].get("rateLimits").is_none());
    }

    #[test]
    fn rate_limit_without_metadata_still_publishes_a_notice() {
        let error = LLMError::RateLimit { metadata: None };
        let update = rate_limit_notice_update("baseten", &error, None).expect("generic rate-limit notice");
        let value = serde_json::to_value(update).expect("notice JSON");
        let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");

        assert_eq!(value["_meta"]["lody"]["notice"]["source"], "provider_rate_limit");
        assert!(message.contains("rate limit"));
        assert!(!message.contains("HTTP"), "a missing status must not be reported as HTTP 429: {message}");
        assert!(!message.contains("Retry-After"));
        assert!(message.contains("no further automatic retry is scheduled"));
    }

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

    #[test]
    fn non_rate_limit_errors_do_not_emit_rate_limit_notices() {
        let error = LLMError::Network {
            message: "connection reset".to_string(),
            metadata: None,
        };
        assert!(rate_limit_notice_update("baseten", &error, None).is_none());
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
