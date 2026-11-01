//! Admission, retry, deadline, and telemetry policy for ACP provider requests.
//!
//! This module carries ACP cancellation, retry, deadline, and telemetry policy
//! for provider calls. Custom-provider concurrency is enforced by the shared
//! router gate in `vtcode-llm` so child and ACP requests use one permit pool.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::debug;
use vtcode_config::TimeoutsConfig;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use vtcode_core::metrics::MetricsCollector;
use vtcode_core::retry::RetryPolicy;

use super::types::SessionCancellation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ProviderClass {
    Builtin,
    Custom,
    Unknown,
}

impl ProviderClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Custom => "custom",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone)]
pub(crate) struct ProviderRequestRuntime {
    provider_name: Arc<str>,
    provider_class: ProviderClass,
    limit: Option<usize>,
    semaphore: Option<Arc<Semaphore>>,
    queue_timeout: Duration,
    retry_policy: RetryPolicy,
    deadline_policy: ProviderDeadlinePolicy,
    counters: Arc<ProviderRequestCounters>,
    metrics: Arc<MetricsCollector>,
}

#[derive(Debug, Default)]
struct ProviderRequestCounters {
    queued: AtomicUsize,
    active: AtomicUsize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderTelemetrySnapshot {
    pub(crate) acp_local_queue_depth: usize,
    pub(crate) active_acp_requests: usize,
    pub(crate) configured_provider_limit: Option<usize>,
    pub(crate) circuit_breaker_state: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderDeadlinePolicy {
    pub(crate) connect: Option<Duration>,
    pub(crate) first_token: Option<Duration>,
    pub(crate) stream_idle: Option<Duration>,
    pub(crate) total_generation: Option<Duration>,
}

impl ProviderDeadlinePolicy {
    fn from_config(config: &CustomProviderRequestPolicyConfig) -> Self {
        Self {
            connect: optional_seconds(config.connect_timeout_seconds),
            first_token: optional_seconds(config.first_token_timeout_seconds),
            stream_idle: optional_seconds(config.stream_idle_timeout_seconds),
            total_generation: optional_seconds(config.total_generation_timeout_seconds),
        }
    }

    fn from_timeouts(config: &TimeoutsConfig) -> Self {
        Self {
            connect: optional_seconds(config.connect_timeout_seconds),
            first_token: optional_seconds(config.first_token_timeout_seconds),
            stream_idle: optional_seconds(config.stream_idle_timeout_seconds),
            total_generation: optional_seconds(config.streaming_ceiling_seconds),
        }
    }
}

fn optional_seconds(seconds: u64) -> Option<Duration> {
    (seconds > 0).then(|| Duration::from_secs(seconds))
}

/// Per-provider policy inputs shared by every runtime construction path.
struct ProviderRuntimeSettings<'a> {
    provider_class: ProviderClass,
    config: &'a CustomProviderRequestPolicyConfig,
    deadline_policy: ProviderDeadlinePolicy,
}

impl ProviderRequestRuntime {
    fn new(
        provider_name: impl Into<Arc<str>>,
        settings: ProviderRuntimeSettings<'_>,
        metrics: Arc<MetricsCollector>,
    ) -> Self {
        let ProviderRuntimeSettings { provider_class, config, deadline_policy } = settings;
        let provider_name = provider_name.into();
        // Request concurrency is enforced once, at the custom-provider router,
        // so child/core and ACP requests consume the same process-wide permits.
        // This runtime still owns ACP cancellation, retry, timeout, and telemetry
        // policy; it must not acquire a second permit around provider calls.
        let semaphore = None;
        let mut retry_policy = RetryPolicy::from_retries(
            config.max_retries,
            Duration::from_millis(config.retry_initial_backoff_ms),
            Duration::from_millis(config.retry_max_backoff_ms),
            2.0,
        );
        if config.retry_jitter {
            retry_policy.jitter = 0.2;
        }

        Self {
            provider_name,
            provider_class,
            limit: config.max_in_flight_requests,
            semaphore,
            queue_timeout: Duration::from_secs(config.queue_timeout_seconds),
            retry_policy,
            deadline_policy,
            counters: Arc::new(ProviderRequestCounters::default()),
            metrics,
        }
    }

    pub(crate) fn retry_policy(&self) -> RetryPolicy {
        self.retry_policy
    }

    pub(crate) fn provider_name(&self) -> &str {
        &self.provider_name
    }

    pub(crate) fn provider_class(&self) -> ProviderClass {
        self.provider_class
    }

    pub(crate) fn deadline_policy(&self) -> ProviderDeadlinePolicy {
        self.deadline_policy
    }

    pub(crate) fn metrics_collector(&self) -> Arc<MetricsCollector> {
        Arc::clone(&self.metrics)
    }

    pub(crate) fn record_attempt_succeeded(&self, duration_ms: u64, first_output_ms: Option<u64>) {
        self.metrics.record_provider_attempt_succeeded(duration_ms, first_output_ms);
    }

    pub(crate) fn record_attempt_failed(&self, duration_ms: u64, first_output_ms: Option<u64>) {
        self.metrics.record_provider_attempt_failed(duration_ms, false, first_output_ms);
    }

    pub(crate) fn record_attempt_cancelled(&self, duration_ms: u64, first_output_ms: Option<u64>) {
        self.metrics.record_provider_attempt_failed(duration_ms, true, first_output_ms);
    }

    pub(crate) fn record_retry_scheduled(&self) {
        self.metrics.record_provider_retry_scheduled();
    }

    pub(crate) fn apply_http_timeouts(&self, timeouts: &mut TimeoutsConfig) {
        timeouts.connect_timeout_seconds = self.deadline_policy.connect.map_or(0, |duration| duration.as_secs());
        let total = self.deadline_policy.total_generation.map_or(0, |duration| duration.as_secs());
        timeouts.default_ceiling_seconds = total;
        timeouts.streaming_ceiling_seconds = total;
    }

    pub(crate) fn telemetry_snapshot(&self) -> ProviderTelemetrySnapshot {
        ProviderTelemetrySnapshot {
            acp_local_queue_depth: self.counters.queued.load(Ordering::Relaxed),
            active_acp_requests: self.counters.active.load(Ordering::Relaxed),
            configured_provider_limit: self.limit,
            circuit_breaker_state: "not_configured",
        }
    }

    fn provider_permit(&self, permit: Option<OwnedSemaphorePermit>) -> ProviderPermit {
        let active_acp_requests = self.counters.active.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        self.metrics.record_provider_attempt_started();
        let snapshot = self.telemetry_snapshot();
        debug!(
            provider = self.provider_name(),
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider request attempt started"
        );
        ProviderPermit {
            _permit: permit,
            provider_name: Arc::clone(&self.provider_name),
            counters: Arc::clone(&self.counters),
            limit: self.limit,
            metrics: Arc::clone(&self.metrics),
        }
    }

    /// Acquires an ACP request permit and updates the runtime's active counters.
    ///
    /// If a local semaphore is present, the request waits up to `queue_timeout`;
    /// cancellation, a closed semaphore or expiry returns the corresponding
    /// [`ProviderAdmissionError`]. The queue-depth guard is released on every
    /// exit, and a successful [`ProviderPermit`] releases active accounting and
    /// its semaphore permit when dropped. With no local semaphore, admission
    /// succeeds immediately without observing cancellation. Custom-provider
    /// concurrency is currently enforced by the shared router, so the ACP
    /// runtime does not acquire a second semaphore permit.
    pub(crate) async fn acquire(
        &self,
        cancellation: &SessionCancellation,
    ) -> Result<ProviderPermit, ProviderAdmissionError> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(self.provider_permit(None));
        };

        let queue_depth = self.counters.queued.fetch_add(1, Ordering::Relaxed).saturating_add(1);
        let queue_guard = ProviderQueueGuard {
            provider_name: Arc::clone(&self.provider_name),
            counters: Arc::clone(&self.counters),
            limit: self.limit,
        };
        let snapshot = self.telemetry_snapshot();
        debug!(
            provider = self.provider_name(),
            acp_local_queue_depth = queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider request waiting for local admission"
        );
        let acquire = semaphore.clone().acquire_owned();
        tokio::select! {
            () = cancellation.cancelled() => Err(ProviderAdmissionError::Cancelled),
            result = tokio::time::timeout(self.queue_timeout, acquire) => {
                match result {
                    Ok(Ok(permit)) => {
                        drop(queue_guard);
                        Ok(self.provider_permit(Some(permit)))
                    },
                    Ok(Err(_closed)) => Err(ProviderAdmissionError::Closed {
                        provider: Arc::clone(&self.provider_name),
                    }),
                    Err(_elapsed) => Err(ProviderAdmissionError::QueueTimeout {
                        provider: Arc::clone(&self.provider_name),
                        limit: self.limit.unwrap_or_default(),
                        waited: self.queue_timeout,
                    }),
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct ProviderPermit {
    _permit: Option<OwnedSemaphorePermit>,
    provider_name: Arc<str>,
    counters: Arc<ProviderRequestCounters>,
    limit: Option<usize>,
    metrics: Arc<MetricsCollector>,
}

impl Drop for ProviderPermit {
    fn drop(&mut self) {
        let active_acp_requests = self.counters.active.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        self.metrics.record_provider_attempt_finished();
        debug!(
            provider = self.provider_name.as_ref(),
            acp_local_queue_depth = self.counters.queued.load(Ordering::Relaxed),
            active_acp_requests,
            configured_provider_limit = ?self.limit,
            circuit_breaker_state = "not_configured",
            "ACP provider request attempt finished"
        );
    }
}

struct ProviderQueueGuard {
    provider_name: Arc<str>,
    counters: Arc<ProviderRequestCounters>,
    limit: Option<usize>,
}

impl Drop for ProviderQueueGuard {
    fn drop(&mut self) {
        let queue_depth = self.counters.queued.fetch_sub(1, Ordering::Relaxed).saturating_sub(1);
        debug!(
            provider = self.provider_name.as_ref(),
            acp_local_queue_depth = queue_depth,
            active_acp_requests = self.counters.active.load(Ordering::Relaxed),
            configured_provider_limit = ?self.limit,
            circuit_breaker_state = "not_configured",
            "ACP provider request left local admission queue"
        );
    }
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProviderAdmissionError {
    #[error("provider request cancelled while waiting for capacity")]
    Cancelled,
    #[error("provider '{provider}' request queue closed")]
    Closed { provider: Arc<str> },
    #[error("provider '{provider}' remained at its per-process limit of {limit} for {waited:?}")]
    QueueTimeout {
        provider: Arc<str>,
        limit: usize,
        waited: Duration,
    },
}

pub(crate) struct ProviderRuntimeRegistry {
    default_runtime: ProviderRequestRuntime,
    custom_runtimes: HashMap<String, ProviderRequestRuntime>,
}

impl ProviderRuntimeRegistry {
    pub(crate) fn new(
        custom_providers: &[CustomProviderConfig],
        timeouts: &TimeoutsConfig,
        metrics: Arc<MetricsCollector>,
    ) -> Self {
        let default_config = CustomProviderRequestPolicyConfig::default();
        let custom_runtimes = custom_providers
            .iter()
            .map(|provider| {
                (
                    provider.name.to_ascii_lowercase(),
                    ProviderRequestRuntime::new(
                        provider.name.clone(),
                        ProviderRuntimeSettings {
                            provider_class: ProviderClass::Custom,
                            config: &provider.request_policy,
                            deadline_policy: ProviderDeadlinePolicy::from_config(&provider.request_policy),
                        },
                        Arc::clone(&metrics),
                    ),
                )
            })
            .collect();

        Self {
            default_runtime: ProviderRequestRuntime::new(
                "default",
                ProviderRuntimeSettings {
                    provider_class: ProviderClass::Unknown,
                    config: &default_config,
                    deadline_policy: ProviderDeadlinePolicy::from_timeouts(timeouts),
                },
                metrics,
            ),
            custom_runtimes,
        }
    }

    pub(crate) fn for_provider(&self, provider_name: &str) -> ProviderRequestRuntime {
        self.custom_runtimes
            .get(&provider_name.to_ascii_lowercase())
            .cloned()
            .unwrap_or_else(|| self.default_runtime.clone())
    }
}

#[cfg(test)]
mod tests;
