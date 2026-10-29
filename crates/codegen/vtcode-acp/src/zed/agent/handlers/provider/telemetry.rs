//! Provider request latency, retry, and failure telemetry.
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, warn};

use super::{
    GenerationFailure, GenerationTelemetry, LLMError, LLMResponse, ProviderMetricContext, ProviderMetricObservation,
    ProviderMetricSink, ProviderMode, ProviderRequestRuntime, RetryDecision, RetryDisposition,
};

#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::zed::agent::handlers) struct ProviderErrorTelemetry {
    pub(in crate::zed::agent::handlers) category: &'static str,
    pub(in crate::zed::agent::handlers) status: Option<u16>,
    pub(in crate::zed::agent::handlers) timeout_phase: Option<&'static str>,
}

pub(in crate::zed::agent::handlers) fn provider_error_telemetry(error: &LLMError) -> ProviderErrorTelemetry {
    let metadata = match error {
        LLMError::Authentication { metadata, .. }
        | LLMError::RateLimit { metadata }
        | LLMError::InvalidRequest { metadata, .. }
        | LLMError::Network { metadata, .. }
        | LLMError::Provider { metadata, .. } => metadata.as_deref(),
    };
    let Some(metadata) = metadata else {
        return ProviderErrorTelemetry {
            category: super::SafeProviderProjection::from_llm_error(error).category(),
            ..Default::default()
        };
    };
    let code = metadata.code.as_deref();
    let timeout_phase = match code {
        Some("acp_timeout_first_token") => Some("first_token"),
        Some("acp_timeout_inter_token_idle") => Some("inter_token_idle"),
        Some("acp_timeout_total_generation") => Some("total_generation"),
        _ => None,
    };
    ProviderErrorTelemetry {
        category: super::SafeProviderProjection::from_llm_error(error).category(),
        status: metadata.status,
        timeout_phase,
    }
}
impl GenerationTelemetry {
    pub(in crate::zed::agent::handlers) fn start(
        runtime: &ProviderRequestRuntime,
        metric_sink: ProviderMetricSink,
        mode: ProviderMode,
    ) -> Self {
        Self {
            metric_sink,
            mode,
            started_at: Instant::now(),
            first_output_at: None,
            estimated_output_tokens: 0,
            runtime: runtime.clone(),
            finished: false,
        }
    }

    pub(in crate::zed::agent::handlers) async fn record_attempt(&self, runtime: &ProviderRequestRuntime) {
        self.metric_sink
            .record(ProviderMetricObservation::Attempt(ProviderMetricContext::request(
                self.mode,
                runtime.provider_class(),
            )))
            .await;
    }

    pub(in crate::zed::agent::handlers) fn observe_output(
        &mut self,
        runtime: &ProviderRequestRuntime,
        delta: &str,
        retry_count: u32,
    ) {
        let estimated_delta = u64::try_from(delta.chars().count().div_ceil(4)).unwrap_or(u64::MAX);
        self.estimated_output_tokens = self.estimated_output_tokens.saturating_add(estimated_delta);
        if self.first_output_at.is_some() {
            return;
        }

        let now = Instant::now();
        self.first_output_at = Some(now);
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider_class = runtime.provider_class().as_str(),
            time_to_first_token_ms = duration_millis(now.duration_since(self.started_at)),
            retry_count,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider produced its first output"
        );
    }

    pub(in crate::zed::agent::handlers) fn has_observed_output(&self) -> bool {
        self.first_output_at.is_some()
    }

    pub(in crate::zed::agent::handlers) async fn complete(
        &mut self,
        runtime: &ProviderRequestRuntime,
        response: &LLMResponse,
        retry_count: u32,
    ) {
        let elapsed = self.started_at.elapsed();
        let elapsed_ms = duration_millis(elapsed).max(1);
        let (output_tokens, token_count_source) = response
            .usage
            .as_ref()
            .filter(|usage| usage.completion_tokens > 0)
            .map(|usage| (u64::from(usage.completion_tokens), "provider"))
            .unwrap_or((self.estimated_output_tokens, "estimated"));
        let tokens_per_second = output_tokens.saturating_mul(1_000) / elapsed_ms;
        runtime.record_attempt_succeeded(
            elapsed_ms,
            self.first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at))),
        );
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider_class = runtime.provider_class().as_str(),
            generation_elapsed_ms = elapsed_ms,
            time_to_first_token_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)))
                .unwrap_or(elapsed_ms),
            ttft_observation = match self.mode { ProviderMode::Buffered => "buffered_response", ProviderMode::Stream => "stream_event" },
            output_tokens,
            token_count_source,
            tokens_per_second,
            retry_count,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider generation completed"
        );
        self.metric_sink
            .record(ProviderMetricObservation::Generation {
                context: ProviderMetricContext::request(self.mode, runtime.provider_class()),
                duration: elapsed,
            })
            .await;
    }

    pub(in crate::zed::agent::handlers) async fn failed(&mut self, failure: GenerationFailure<'_>) {
        let GenerationFailure {
            runtime,
            retry_count,
            retry_disposition,
            retry_delay,
            error,
        } = failure;
        #[cfg(test)]
        crate::zed::agent::handlers::responses_tests::record_provider_failure(error, runtime);
        let elapsed_ms = duration_millis(self.started_at.elapsed());
        let first_output_ms = self
            .first_output_at
            .map(|first| duration_millis(first.duration_since(self.started_at)));
        runtime.record_attempt_failed(elapsed_ms, first_output_ms);
        if retry_disposition == RetryDisposition::RetryScheduled {
            runtime.record_retry_scheduled();
        }
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        let error_telemetry = provider_error_telemetry(error);
        warn!(
            provider_class = runtime.provider_class().as_str(),
            generation_elapsed_ms = duration_millis(self.started_at.elapsed()),
            retry_count,
            max_retries = runtime.retry_policy().max_attempts.saturating_sub(1),
            retry_disposition = retry_disposition.as_str(),
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            error_category = error_telemetry.category,
            error_status = ?error_telemetry.status,
            provider_timeout_phase = error_telemetry.timeout_phase,
            "ACP provider generation attempt failed"
        );
        let error_category = vtcode_commons::ErrorCategory::from(error);
        if error_category == vtcode_commons::ErrorCategory::RateLimit {
            self.metric_sink
                .record(ProviderMetricObservation::RateLimited(ProviderMetricContext::failure(
                    self.mode,
                    runtime.provider_class(),
                    error_category,
                    retry_disposition,
                )))
                .await;
        }
        if retry_disposition == RetryDisposition::RetryScheduled {
            self.metric_sink
                .record(ProviderMetricObservation::Retry(ProviderMetricContext::failure(
                    self.mode,
                    runtime.provider_class(),
                    error_category,
                    RetryDisposition::RetryScheduled,
                )))
                .await;
            if let Some(delay) = retry_delay {
                self.metric_sink
                    .record(ProviderMetricObservation::RetryDelay {
                        context: ProviderMetricContext::failure(
                            self.mode,
                            runtime.provider_class(),
                            error_category,
                            RetryDisposition::RetryScheduled,
                        ),
                        delay,
                    })
                    .await;
            }
        }
    }
}

impl Drop for GenerationTelemetry {
    fn drop(&mut self) {
        if !self.finished {
            let first_output_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)));
            self.runtime
                .record_attempt_cancelled(duration_millis(self.started_at.elapsed()), first_output_ms);
            self.finished = true;
        }
    }
}

pub(in crate::zed::agent::handlers) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub(in crate::zed::agent::handlers) fn retry_disposition(decision: &RetryDecision) -> RetryDisposition {
    if decision.retryable {
        RetryDisposition::RetryScheduled
    } else if decision.category.is_retryable() {
        RetryDisposition::RetryExhausted
    } else {
        RetryDisposition::NonRetryable
    }
}
