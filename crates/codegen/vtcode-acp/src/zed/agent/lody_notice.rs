use crate::acp;
use serde_json::{Map, Value};
use std::time::Duration;
use tracing::warn;
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
    let metadata = rate_limit_metadata(error)?;
    let retry_after = metadata.retry_after.as_deref();
    let mut message = format!("{provider} returned HTTP 429 (rate limited)");
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

fn rate_limit_metadata(error: &LLMError) -> Option<&LLMErrorMetadata> {
    let metadata = match error {
        LLMError::Authentication { metadata, .. }
        | LLMError::RateLimit { metadata }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => metadata.as_deref(),
    }?;
    matches!(error, LLMError::RateLimit { .. })
        .then_some(metadata)
        .or_else(|| (metadata.status == Some(429)).then_some(metadata))
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
