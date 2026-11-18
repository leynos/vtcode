//! Publishes provider rate-limit warnings as ACP session-info notices carrying
//! `_meta.lody.notice`, reporting only retry information the provider actually
//! supplied and never a fabricated quota window.

use crate::acp;
use diapason_core::llm::provider::{LLMError, LLMErrorMetadata};
use serde_json::{Map, Value};
use std::time::Duration;
use tracing::{Instrument, field, info_span};

use super::ZedAgent;
use super::redaction::{elapsed_millis, log_notice_publish_failure, session_correlation};

impl ZedAgent {
    /// Publish a warning notice when `error` reports a provider rate limit.
    ///
    /// `retry_delay` is the delay Diapason actually scheduled, or `None` when no
    /// further attempt will be made. Errors that report no rate limit publish
    /// nothing.
    pub(super) async fn publish_rate_limit_notice(
        &self,
        session_id: &acp::SessionId,
        provider: &str,
        error: &LLMError,
        retry_delay: Option<Duration>,
    ) {
        // An error that reports no rate limit publishes nothing, so it is not
        // an attempt and must not move the failure ratio.
        let Some(update) = rate_limit_notice_update(provider, error, retry_delay) else {
            return;
        };
        let session_correlation = session_correlation(&session_id.to_string());
        let span = info_span!(
            "acp_notice_publish",
            operation = "provider_rate_limit_notice",
            stage = "publish_rate_limit_notice",
            session_correlation = %session_correlation,
            outcome = field::Empty,
            elapsed_ms = field::Empty,
        );
        let started_at = self.publication_clock.now();
        // Record on this span's own handle: `Span::current()` would fall back
        // to an enclosing span when a filter disables this one, overwriting
        // that span's fields.
        let publish_span = span.clone();
        async move {
            let result = self.send_update(session_id, update).await;
            let finished_at = self.publication_clock.now();
            let sent = result.is_ok();
            let _outcome_recorded = publish_span.record("outcome", if sent { "sent" } else { "failed" });
            let _elapsed_ms_recorded = publish_span.record("elapsed_ms", elapsed_millis(started_at, finished_at));
            self.notice_recorder.record(sent);
            if let Err(error) = &result {
                // The transport error and the raw session id are both
                // operator-sensitive, so only fixed fields and a bounded
                // correlation digest reach the log.
                log_notice_publish_failure(error, &session_correlation);
            }
        }
        .instrument(span)
        .await;
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
    if let Some(retry_after) = retry_after.filter(|value| is_safe_retry_after(value)) {
        message.push_str(&format!("; provider Retry-After: {retry_after}"));
    }
    if let Some(delay) = retry_delay {
        message.push_str(&format!("; Diapason will retry in {:.1}s", delay.as_secs_f64()));
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

/// Maximum provider-supplied `Retry-After` hint length, in bytes.
///
/// The hint is echoed into a client-visible notice, so it is bounded and must
/// not carry control characters that could forge or break the message.
const RETRY_AFTER_MAX_BYTES: usize = 64;

/// Report whether a provider `Retry-After` hint is safe to echo.
///
/// RFC 9110 defines the header as either `delay-seconds` or an HTTP-date, so
/// anything else - free text such as `not available`, a signed number, or an
/// exponent - is omitted rather than presented to the client as a retry hint.
/// Fractional seconds are accepted as an explicit extension to the HTTP
/// grammar because the providers wired into this bridge report them.
fn is_safe_retry_after(value: &str) -> bool {
    value.len() <= RETRY_AFTER_MAX_BYTES
        && !value.chars().any(char::is_control)
        && (is_retry_after_delay(value) || chrono::DateTime::parse_from_rfc2822(value).is_ok())
}

/// Report whether `value` is a non-negative delay in seconds.
///
/// The integer part must be present and numeric. The optional fractional part
/// must also be present and numeric, which rejects signs, exponents, and the
/// special values a plain float parse would accept.
fn is_retry_after_delay(value: &str) -> bool {
    let (whole, fraction) = match value.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (value, None),
    };
    !whole.is_empty()
        && whole.bytes().all(|byte| byte.is_ascii_digit())
        && fraction.is_none_or(|fraction| !fraction.is_empty() && fraction.bytes().all(|byte| byte.is_ascii_digit()))
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
    //! Covers the rate-limit notice body built from a provider error.
    //!
    //! A notice may describe only what the provider supplied and what VT Code
    //! actually scheduled, so these tests pin the status, the `Retry-After`
    //! echo, the retry claim, and the errors that publish nothing.

    use super::*;
    use diapason_core::llm::provider::LLMErrorMetadata;

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

        assert_eq!(value["sessionUpdate"], "session_info_update");
        assert_eq!(value["_meta"]["lody"]["notice"]["level"], "warning");
        assert_eq!(value["_meta"]["lody"]["notice"]["source"], "provider_rate_limit");
        let message = value["_meta"]["lody"]["notice"]["message"].as_str().expect("notice message");
        assert!(message.contains("HTTP 429"));
        assert!(message.contains("Retry-After: 17"));
        assert!(message.contains("retry in 20.0s"));
        assert!(value["_meta"]["lody"].get("rateLimits").is_none());
    }

    /// Assert an unsafe provider `Retry-After` hint is omitted from the notice.
    #[test]
    fn unsafe_retry_after_hint_is_omitted() {
        let cases = [
            ("x".repeat(RETRY_AFTER_MAX_BYTES + 1), "over-long hint"),
            ("17\r\nforged: yes".to_string(), "control characters"),
            ("not available".to_string(), "free text that is not a delay or date"),
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

    /// Assert only HTTP-grammar retry hints are safe to echo.
    #[test]
    fn retry_after_hint_accepts_delays_and_http_dates_only() {
        for hint in ["17", "0.5", "0.2", "Sun, 06 Nov 1994 08:49:37 GMT"] {
            assert!(is_safe_retry_after(hint), "{hint} must be a valid retry hint");
        }
        for hint in ["not available", "1e5", "-1", "+1", ".5", "1.", "", "17 30"] {
            assert!(!is_safe_retry_after(hint), "{hint} must not be echoed as a retry hint");
        }
    }

    /// Assert a rate limit without metadata still publishes a notice.
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
