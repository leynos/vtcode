//! Projects provider failures into bounded, client-safe ACP telemetry fields
//! without forwarding raw provider diagnostics.

use std::time::Duration;

use hashbrown::HashMap;
use vtcode_commons::ErrorCategory;
use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

use crate::zed::provider_runtime::ProviderClass;

const PROVIDER_ATTEMPT_TOTAL: &str = "vtcode.acp.provider_attempt_total";
const PROVIDER_RETRY_TOTAL: &str = "vtcode.acp.provider_retry_total";
const PROVIDER_RETRY_DELAY_MS: &str = "vtcode.acp.provider_retry_delay_ms";
const PROVIDER_GENERATION_MS: &str = "vtcode.acp.provider_generation_ms";
const PROVIDER_RATE_LIMITED_TOTAL: &str = "vtcode.acp.provider_rate_limited_total";
const RATE_LIMIT_NOTIFICATION_FAILURE_TOTAL: &str = "vtcode.acp.rate_limit_notification_failure_total";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ProviderMode {
    Buffered,
    Stream,
}

impl ProviderMode {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Buffered => "buffered",
            Self::Stream => "stream",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RetryDisposition {
    RetryScheduled,
    RetryExhausted,
    NonRetryable,
    PartialOutputVisible,
}

impl RetryDisposition {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::RetryScheduled => "retry_scheduled",
            Self::RetryExhausted => "retry_exhausted",
            Self::NonRetryable => "non_retryable",
            Self::PartialOutputVisible => "partial_output_visible",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProviderMetric {
    ProviderAttemptTotal,
    ProviderRetryTotal,
    ProviderRetryDelayMs,
    ProviderGenerationMs,
    ProviderRateLimitedTotal,
    RateLimitNotificationFailureTotal,
}

impl ProviderMetric {
    const fn name(self) -> &'static str {
        match self {
            Self::ProviderAttemptTotal => PROVIDER_ATTEMPT_TOTAL,
            Self::ProviderRetryTotal => PROVIDER_RETRY_TOTAL,
            Self::ProviderRetryDelayMs => PROVIDER_RETRY_DELAY_MS,
            Self::ProviderGenerationMs => PROVIDER_GENERATION_MS,
            Self::ProviderRateLimitedTotal => PROVIDER_RATE_LIMITED_TOTAL,
            Self::RateLimitNotificationFailureTotal => RATE_LIMIT_NOTIFICATION_FAILURE_TOTAL,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProviderMetricTags {
    mode: Option<ProviderMode>,
    provider_class: ProviderClass,
    error_category: Option<ErrorCategory>,
    retry_disposition: Option<RetryDisposition>,
}

impl ProviderMetricTags {
    const fn new(
        mode: Option<ProviderMode>,
        provider_class: ProviderClass,
        error_category: Option<ErrorCategory>,
        retry_disposition: Option<RetryDisposition>,
    ) -> Self {
        Self {
            mode,
            provider_class,
            error_category,
            retry_disposition,
        }
    }

    fn into_map(self) -> HashMap<String, String> {
        let mut tags = HashMap::new();
        if let Some(mode) = self.mode {
            let _previous_mode = tags.insert("mode".to_owned(), mode.as_str().to_owned());
        }
        let _previous_provider_class =
            tags.insert("provider_class".to_owned(), self.provider_class.as_str().to_owned());
        if let Some(error_category) = self.error_category {
            let _previous_error_category = tags.insert("error_category".to_owned(), error_category.as_str().to_owned());
        }
        if let Some(retry_disposition) = self.retry_disposition {
            let _previous_retry_disposition =
                tags.insert("retry_disposition".to_owned(), retry_disposition.as_str().to_owned());
        }
        tags
    }
}

/// Request-owned sink for the finite ACP provider metric vocabulary.
///
/// Production records use the process-wide perf pipeline. Tests provide their
/// own recorder, so recording remains awaited and no global test hook is
/// needed.
#[derive(Clone, Debug)]
pub(super) enum ProviderMetricSink {
    Process,
    #[cfg(test)]
    Test(std::sync::Arc<vtcode_core::telemetry::perf::TestPerfRecorder>),
}

impl ProviderMetricSink {
    pub(super) const fn production() -> Self {
        Self::Process
    }

    #[cfg(test)]
    pub(super) fn test(recorder: std::sync::Arc<vtcode_core::telemetry::perf::TestPerfRecorder>) -> Self {
        Self::Test(recorder)
    }

    pub(super) async fn record_provider_attempt(&self, mode: ProviderMode, provider_class: ProviderClass) {
        self.record_value(
            ProviderMetric::ProviderAttemptTotal,
            1.0,
            ProviderMetricTags::new(Some(mode), provider_class, None, None),
        )
        .await;
    }

    pub(super) async fn record_provider_retry(
        &self,
        mode: ProviderMode,
        provider_class: ProviderClass,
        error_category: ErrorCategory,
    ) {
        self.record_value(
            ProviderMetric::ProviderRetryTotal,
            1.0,
            ProviderMetricTags::new(
                Some(mode),
                provider_class,
                Some(error_category),
                Some(RetryDisposition::RetryScheduled),
            ),
        )
        .await;
    }

    pub(super) async fn record_provider_retry_delay(
        &self,
        mode: ProviderMode,
        provider_class: ProviderClass,
        error_category: ErrorCategory,
        delay: Duration,
    ) {
        self.record_value(
            ProviderMetric::ProviderRetryDelayMs,
            delay.as_secs_f64() * 1000.0,
            ProviderMetricTags::new(
                Some(mode),
                provider_class,
                Some(error_category),
                Some(RetryDisposition::RetryScheduled),
            ),
        )
        .await;
    }

    pub(super) async fn record_provider_generation(
        &self,
        mode: ProviderMode,
        provider_class: ProviderClass,
        duration: Duration,
    ) {
        self.record_duration(
            ProviderMetric::ProviderGenerationMs,
            duration,
            ProviderMetricTags::new(Some(mode), provider_class, None, None),
        )
        .await;
    }

    pub(super) async fn record_provider_rate_limited(
        &self,
        mode: ProviderMode,
        provider_class: ProviderClass,
        error_category: ErrorCategory,
        retry_disposition: RetryDisposition,
    ) {
        self.record_value(
            ProviderMetric::ProviderRateLimitedTotal,
            1.0,
            ProviderMetricTags::new(Some(mode), provider_class, Some(error_category), Some(retry_disposition)),
        )
        .await;
    }

    pub(super) async fn record_rate_limit_notification_failure(
        &self,
        provider_class: ProviderClass,
        error_category: ErrorCategory,
    ) {
        self.record_value(
            ProviderMetric::RateLimitNotificationFailureTotal,
            1.0,
            ProviderMetricTags::new(None, provider_class, Some(error_category), None),
        )
        .await;
    }

    async fn record_value(&self, metric: ProviderMetric, value: f64, tags: ProviderMetricTags) {
        let name = metric.name();
        let tags = tags.into_map();
        match self {
            Self::Process => vtcode_core::telemetry::perf::record_value(name, value, tags),
            #[cfg(test)]
            Self::Test(recorder) => {
                recorder
                    .record_value(name, value, tags)
                    .await
                    .expect("record test provider metric");
            }
        }
    }

    async fn record_duration(&self, metric: ProviderMetric, duration: Duration, tags: ProviderMetricTags) {
        let name = metric.name();
        let tags = tags.into_map();
        match self {
            Self::Process => vtcode_core::telemetry::perf::record_duration(name, duration, tags),
            #[cfg(test)]
            Self::Test(recorder) => {
                recorder
                    .record_value(name, duration.as_secs_f64() * 1000.0, tags)
                    .await
                    .expect("record test provider metric");
            }
        }
    }
}

/// The bounded provider failure detail that ACP may expose to a client or
/// structured tracing. Provider diagnostics remain inside the provider layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SafeProviderProjection {
    category: &'static str,
    status: Option<u16>,
}

impl SafeProviderProjection {
    pub(super) fn from_llm_error(error: &LLMError) -> Self {
        Self {
            category: ErrorCategory::from(error).as_str(),
            status: error_metadata(error).and_then(|metadata| metadata.status),
        }
    }

    pub(super) const fn execution_error() -> Self {
        Self {
            category: ErrorCategory::ExecutionError.as_str(),
            status: None,
        }
    }

    pub(super) const fn category(self) -> &'static str {
        self.category
    }

    pub(super) const fn status(self) -> Option<u16> {
        self.status
    }

    pub(super) fn client_message(self) -> String {
        match self.status {
            Some(status) => format!("Provider failure category: {} (HTTP status {status}).", self.category),
            None => format!("Provider failure category: {}.", self.category),
        }
    }
}

fn error_metadata(error: &LLMError) -> Option<&LLMErrorMetadata> {
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
    use super::{ProviderMetric, ProviderMetricTags, ProviderMode, RetryDisposition, SafeProviderProjection};
    use crate::zed::provider_runtime::ProviderClass;
    use vtcode_commons::ErrorCategory;
    use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

    #[test]
    fn safe_projection_excludes_provider_diagnostics() {
        let marker = "provider-marker://model/request/session/error-code/body";
        let error = LLMError::Provider {
            message: marker.to_string(),
            metadata: Some(LLMErrorMetadata::new(
                marker,
                Some(429),
                Some(marker.to_string()),
                Some(marker.to_string()),
                Some(marker.to_string()),
                None,
                Some(marker.to_string()),
            )),
        };

        let projection = SafeProviderProjection::from_llm_error(&error);

        assert_eq!(projection.category(), "Rate limit exceeded");
        assert_eq!(projection.status(), Some(429));
        assert!(!projection.client_message().contains(marker));
    }

    #[test]
    fn provider_metric_descriptors_use_exact_finite_vocabulary() {
        let descriptors = [
            ProviderMetric::ProviderAttemptTotal,
            ProviderMetric::ProviderRetryTotal,
            ProviderMetric::ProviderRetryDelayMs,
            ProviderMetric::ProviderGenerationMs,
            ProviderMetric::ProviderRateLimitedTotal,
            ProviderMetric::RateLimitNotificationFailureTotal,
        ];
        let names: Vec<_> = descriptors.into_iter().map(ProviderMetric::name).collect();

        assert_eq!(
            names,
            vec![
                "vtcode.acp.provider_attempt_total",
                "vtcode.acp.provider_retry_total",
                "vtcode.acp.provider_retry_delay_ms",
                "vtcode.acp.provider_generation_ms",
                "vtcode.acp.provider_rate_limited_total",
                "vtcode.acp.rate_limit_notification_failure_total",
            ]
        );
    }

    #[test]
    fn provider_metric_tags_exclude_raw_diagnostics() {
        let marker = "provider-marker://model/request/session/error-code/body";
        let tags = ProviderMetricTags::new(
            Some(ProviderMode::Stream),
            ProviderClass::Custom,
            Some(ErrorCategory::RateLimit),
            Some(RetryDisposition::PartialOutputVisible),
        )
        .into_map();

        assert_eq!(tags.len(), 4);
        assert_eq!(tags.get("mode").map(String::as_str), Some("stream"));
        assert_eq!(tags.get("provider_class").map(String::as_str), Some("custom"));
        assert_eq!(tags.get("error_category").map(String::as_str), Some("Rate limit exceeded"));
        assert_eq!(tags.get("retry_disposition").map(String::as_str), Some("partial_output_visible"));
        assert!(tags.iter().all(|(key, value)| !key.contains(marker) && !value.contains(marker)));
    }
}
