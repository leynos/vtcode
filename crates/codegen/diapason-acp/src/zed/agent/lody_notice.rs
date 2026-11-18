//! Publishes provider rate-limit warnings as ACP session-info notices carrying
//! `_meta.lody.notice`, reporting only retry information the provider actually
//! supplied and never a fabricated quota window.

use crate::acp;
use diapason_commons::llm::RateLimitMetadata;
use diapason_core::llm::provider::{LLMError, LLMErrorMetadata};
use serde_json::{Map, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{Instrument, field, info_span};

use super::ZedAgent;
use super::provider_telemetry::{ProviderMetricContext, ProviderMetricObservation, ProviderMetricSink};
use super::redaction::{elapsed_millis, log_notice_publish_failure, session_correlation};
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
        self.publish_rate_limit_notice_at(ObservedRateLimitNotice {
            session_id,
            provider,
            error,
            retry_delay,
            observed_at: SystemTime::now(),
        })
        .await;
    }

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
        async {
            let result = self.send_update(session_id, update).await;
            let finished_at = self.publication_clock.now();
            let span = tracing::Span::current();
            let sent = result.is_ok();
            let _outcome_recorded = span.record("outcome", if sent { "sent" } else { "failed" });
            let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_millis(started_at, finished_at));
            self.notice_recorder.record(sent);
            if let Err(error) = &result {
                metric_sink
                    .record(ProviderMetricObservation::RateLimitNotificationFailure(
                        ProviderMetricContext::rate_limit_notification_failure(provider_class, NOTICE_OPERATION),
                    ))
                    .await;
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
    if let Some(limits) = metadata.and_then(|metadata| metadata.rate_limit.as_ref()) {
        append_rate_limit_details(&mut message, limits);
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
fn is_safe_retry_after(value: &str) -> bool {
    value.len() <= RETRY_AFTER_MAX_BYTES && !value.chars().any(char::is_control)
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
mod tests;
