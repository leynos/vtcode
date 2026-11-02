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
const PROMPT_GATE_QUEUE_DEPTH: &str = "vtcode.acp.prompt_gate_queue_depth";
const PROMPT_GATE_WAIT_MS: &str = "vtcode.acp.prompt_gate_wait_ms";
const PROMPT_OUTCOME_TOTAL: &str = "vtcode.acp.prompt_outcome_total";

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

/// Fixed outcome vocabulary for a finished ACP prompt turn.
///
/// Only the sanitized failure reasons ACP produces itself get their own
/// series; every other error is grouped as `failed`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PromptOutcome {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    EmptyResponse,
    ToolExecutionFailed,
    Failed,
}

impl PromptOutcome {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::EmptyResponse => "empty_response",
            Self::ToolExecutionFailed => "tool_execution_failed",
            Self::Failed => "failed",
        }
    }
}

/// Bounded observations for prompt scheduling and completion.
///
/// These carry no request data: the only labels are the prompt mode and the
/// closed [`PromptOutcome`] vocabulary.
#[derive(Clone, Copy, Debug)]
pub(super) enum PromptMetricObservation {
    GateQueueDepth { depth: usize },
    GateWait { duration: Duration },
    Outcome { mode: ProviderMode, outcome: PromptOutcome },
}

impl PromptMetricObservation {
    /// Projects a prompt observation into its fixed metric name, value, and
    /// labels.
    fn into_record(self) -> (&'static str, f64, HashMap<String, String>) {
        match self {
            Self::GateQueueDepth { depth } => {
                let depth = u32::try_from(depth).map_or(f64::from(u32::MAX), f64::from);
                (PROMPT_GATE_QUEUE_DEPTH, depth, HashMap::new())
            }
            Self::GateWait { duration } => (PROMPT_GATE_WAIT_MS, duration.as_secs_f64() * 1000.0, HashMap::new()),
            Self::Outcome { mode, outcome } => {
                let mut tags = HashMap::new();
                let _previous_mode = tags.insert("mode".to_owned(), mode.as_str().to_owned());
                let _previous_outcome = tags.insert("outcome".to_owned(), outcome.as_str().to_owned());
                (PROMPT_OUTCOME_TOTAL, 1.0, tags)
            }
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
pub(super) struct ProviderMetricContext {
    mode: Option<ProviderMode>,
    provider_class: ProviderClass,
    error_category: Option<ErrorCategory>,
    retry_disposition: Option<RetryDisposition>,
}

impl ProviderMetricContext {
    pub(super) const fn request(mode: ProviderMode, provider_class: ProviderClass) -> Self {
        Self {
            mode: Some(mode),
            provider_class,
            error_category: None,
            retry_disposition: None,
        }
    }

    pub(super) const fn failure(
        mode: ProviderMode,
        provider_class: ProviderClass,
        error_category: ErrorCategory,
        retry_disposition: RetryDisposition,
    ) -> Self {
        Self {
            mode: Some(mode),
            provider_class,
            error_category: Some(error_category),
            retry_disposition: Some(retry_disposition),
        }
    }

    pub(super) const fn rate_limit_notification_failure(provider_class: ProviderClass) -> Self {
        Self {
            mode: None,
            provider_class,
            error_category: Some(ErrorCategory::ExecutionError),
            retry_disposition: None,
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

/// Closed observations emitted by the ACP provider retry paths.
///
/// Each variant owns the metric name, value semantics, and bounded context,
/// which keeps callers from constructing arbitrary provider metric labels.
#[derive(Clone, Copy, Debug)]
pub(super) enum ProviderMetricObservation {
    Attempt(ProviderMetricContext),
    Retry(ProviderMetricContext),
    RetryDelay {
        context: ProviderMetricContext,
        delay: Duration,
    },
    Generation {
        context: ProviderMetricContext,
        duration: Duration,
    },
    RateLimited(ProviderMetricContext),
    RateLimitNotificationFailure(ProviderMetricContext),
}

#[derive(Clone, Copy, Debug)]
struct ProviderMetricRecord {
    metric: ProviderMetric,
    value: f64,
    context: ProviderMetricContext,
}

impl ProviderMetricObservation {
    fn into_record(self) -> ProviderMetricRecord {
        match self {
            Self::Attempt(context) => ProviderMetricRecord {
                metric: ProviderMetric::ProviderAttemptTotal,
                value: 1.0,
                context,
            },
            Self::Retry(context) => ProviderMetricRecord {
                metric: ProviderMetric::ProviderRetryTotal,
                value: 1.0,
                context,
            },
            Self::RetryDelay { context, delay } => ProviderMetricRecord {
                metric: ProviderMetric::ProviderRetryDelayMs,
                value: delay.as_secs_f64() * 1000.0,
                context,
            },
            Self::Generation { context, duration } => ProviderMetricRecord {
                metric: ProviderMetric::ProviderGenerationMs,
                value: duration.as_secs_f64() * 1000.0,
                context,
            },
            Self::RateLimited(context) => ProviderMetricRecord {
                metric: ProviderMetric::ProviderRateLimitedTotal,
                value: 1.0,
                context,
            },
            Self::RateLimitNotificationFailure(context) => ProviderMetricRecord {
                metric: ProviderMetric::RateLimitNotificationFailureTotal,
                value: 1.0,
                context,
            },
        }
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

    pub(super) async fn record(&self, observation: ProviderMetricObservation) {
        let ProviderMetricRecord { metric, value, context } = observation.into_record();
        self.emit(metric.name(), value, context.into_map()).await;
    }

    /// Records a prompt scheduling or completion observation.
    pub(super) async fn record_prompt(&self, observation: PromptMetricObservation) {
        let (name, value, tags) = observation.into_record();
        self.emit(name, value, tags).await;
    }

    async fn emit(&self, name: &'static str, value: f64, tags: HashMap<String, String>) {
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
    use super::{
        PromptMetricObservation, PromptOutcome, ProviderMetric, ProviderMetricContext, ProviderMode, RetryDisposition,
        SafeProviderProjection,
    };
    use crate::zed::provider_runtime::ProviderClass;
    use std::time::Duration;
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
        let tags = ProviderMetricContext::failure(
            ProviderMode::Stream,
            ProviderClass::Custom,
            ErrorCategory::RateLimit,
            RetryDisposition::PartialOutputVisible,
        )
        .into_map();

        assert_eq!(tags.len(), 4);
        assert_eq!(tags.get("mode").map(String::as_str), Some("stream"));
        assert_eq!(tags.get("provider_class").map(String::as_str), Some("custom"));
        assert_eq!(tags.get("error_category").map(String::as_str), Some("Rate limit exceeded"));
        assert_eq!(tags.get("retry_disposition").map(String::as_str), Some("partial_output_visible"));
        assert!(tags.iter().all(|(key, value)| !key.contains(marker) && !value.contains(marker)));
    }

    #[test]
    fn prompt_metric_descriptors_use_exact_finite_vocabulary() {
        let records: Vec<_> = [
            PromptMetricObservation::GateQueueDepth { depth: 2 },
            PromptMetricObservation::GateWait { duration: Duration::from_millis(250) },
            PromptMetricObservation::Outcome {
                mode: ProviderMode::Stream,
                outcome: PromptOutcome::Cancelled,
            },
        ]
        .into_iter()
        .map(PromptMetricObservation::into_record)
        .collect();

        assert_eq!(records[0].0, "vtcode.acp.prompt_gate_queue_depth");
        assert_eq!(records[0].1.to_bits(), 2.0_f64.to_bits());
        assert!(records[0].2.is_empty());
        assert_eq!(records[1].0, "vtcode.acp.prompt_gate_wait_ms");
        assert_eq!(records[1].1.to_bits(), 250.0_f64.to_bits());
        assert!(records[1].2.is_empty());
        assert_eq!(records[2].0, "vtcode.acp.prompt_outcome_total");
        assert_eq!(records[2].1.to_bits(), 1.0_f64.to_bits());
        assert_eq!(records[2].2.len(), 2);
        assert_eq!(records[2].2.get("mode").map(String::as_str), Some("stream"));
        assert_eq!(records[2].2.get("outcome").map(String::as_str), Some("cancelled"));
    }

    #[test]
    fn prompt_outcomes_use_a_fixed_vocabulary() {
        let labels: Vec<_> = [
            PromptOutcome::EndTurn,
            PromptOutcome::MaxTokens,
            PromptOutcome::MaxTurnRequests,
            PromptOutcome::Refusal,
            PromptOutcome::Cancelled,
            PromptOutcome::EmptyResponse,
            PromptOutcome::ToolExecutionFailed,
            PromptOutcome::Failed,
        ]
        .into_iter()
        .map(PromptOutcome::as_str)
        .collect();

        assert_eq!(
            labels,
            vec![
                "end_turn",
                "max_tokens",
                "max_turn_requests",
                "refusal",
                "cancelled",
                "empty_response",
                "tool_execution_failed",
                "failed",
            ]
        );
    }
}
