//! Provider request latency, retry, and failure telemetry.
use std::time::Duration;
use tokio::time::Instant;
use tracing::{info, warn};

use super::{GenerationTelemetry, LLMError, LLMResponse, ProviderRequestRuntime, RetryDecision};

#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::zed::agent::handlers) struct ProviderErrorTelemetry<'a> {
    pub(in crate::zed::agent::handlers) code: Option<&'a str>,
    pub(in crate::zed::agent::handlers) status: Option<u16>,
    pub(in crate::zed::agent::handlers) timeout_phase: Option<&'static str>,
}

pub(in crate::zed::agent::handlers) fn provider_error_telemetry(error: &LLMError) -> ProviderErrorTelemetry<'_> {
    let LLMError::Network { metadata: Some(metadata), .. } = error else {
        return ProviderErrorTelemetry::default();
    };
    let code = metadata.code.as_deref();
    let timeout_phase = match code {
        Some("acp_timeout_first_token") => Some("first_token"),
        Some("acp_timeout_inter_token_idle") => Some("inter_token_idle"),
        Some("acp_timeout_total_generation") => Some("total_generation"),
        _ => None,
    };
    ProviderErrorTelemetry {
        code: if timeout_phase.is_some() { None } else { code },
        status: metadata.status,
        timeout_phase,
    }
}
impl GenerationTelemetry {
    pub(in crate::zed::agent::handlers) fn start(runtime: &ProviderRequestRuntime) -> Self {
        Self {
            started_at: Instant::now(),
            first_output_at: None,
            estimated_output_tokens: 0,
            runtime: runtime.clone(),
            finished: false,
        }
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
            provider = runtime.provider_name(),
            time_to_first_token_ms = duration_millis(now.duration_since(self.started_at)),
            retry_count,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider produced its first output"
        );
    }

    pub(in crate::zed::agent::handlers) fn complete(
        &mut self,
        runtime: &ProviderRequestRuntime,
        response: &LLMResponse,
        retry_count: u32,
        buffered: bool,
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
            provider = runtime.provider_name(),
            generation_elapsed_ms = elapsed_ms,
            time_to_first_token_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)))
                .unwrap_or(elapsed_ms),
            ttft_observation = if buffered { "buffered_response" } else { "stream_event" },
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
    }

    pub(in crate::zed::agent::handlers) fn failed(
        &mut self,
        runtime: &ProviderRequestRuntime,
        retry_count: u32,
        retry_disposition: &'static str,
        error: &LLMError,
    ) {
        let elapsed_ms = duration_millis(self.started_at.elapsed());
        let first_output_ms = self
            .first_output_at
            .map(|first| duration_millis(first.duration_since(self.started_at)));
        runtime.record_attempt_failed(elapsed_ms, first_output_ms);
        if retry_disposition == "retry_scheduled" {
            runtime.record_retry_scheduled();
        }
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        let error_telemetry = provider_error_telemetry(error);
        warn!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = duration_millis(self.started_at.elapsed()),
            retry_count,
            max_retries = runtime.retry_policy().max_attempts.saturating_sub(1),
            retry_disposition,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            provider_error_code = ?error_telemetry.code,
            provider_error_status = ?error_telemetry.status,
            provider_timeout_phase = error_telemetry.timeout_phase,
            "ACP provider generation attempt failed"
        );
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

pub(in crate::zed::agent::handlers) fn retry_disposition(decision: &RetryDecision) -> &'static str {
    if decision.retryable {
        "retry_scheduled"
    } else if decision.category.is_retryable() {
        "retry_exhausted"
    } else {
        "non_retryable"
    }
}
