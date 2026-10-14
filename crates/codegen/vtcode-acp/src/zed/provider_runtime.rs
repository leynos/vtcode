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

#[derive(Clone)]
pub(crate) struct ProviderRequestRuntime {
    provider_name: Arc<str>,
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

impl ProviderRequestRuntime {
    fn new(
        provider_name: impl Into<Arc<str>>,
        config: &CustomProviderRequestPolicyConfig,
        deadline_policy: ProviderDeadlinePolicy,
        metrics: Arc<MetricsCollector>,
    ) -> Self {
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
                        &provider.request_policy,
                        ProviderDeadlinePolicy::from_config(&provider.request_policy),
                        Arc::clone(&metrics),
                    ),
                )
            })
            .collect();

        Self {
            default_runtime: ProviderRequestRuntime::new(
                "default",
                &default_config,
                ProviderDeadlinePolicy::from_timeouts(timeouts),
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
mod tests {
    use super::*;

    fn runtime(limit: usize, queue_timeout: Duration) -> ProviderRequestRuntime {
        ProviderRequestRuntime {
            provider_name: Arc::from("test"),
            limit: Some(limit),
            semaphore: Some(Arc::new(Semaphore::new(limit))),
            queue_timeout,
            retry_policy: RetryPolicy::default(),
            deadline_policy: ProviderDeadlinePolicy::from_config(&CustomProviderRequestPolicyConfig::default()),
            counters: Arc::new(ProviderRequestCounters::default()),
            metrics: Arc::new(MetricsCollector::new()),
        }
    }

    #[tokio::test]
    async fn local_limit_blocks_until_the_active_request_finishes() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = SessionCancellation::default();
        let first = runtime.acquire(&cancellation).await.expect("first permit");
        assert_eq!(runtime.telemetry_snapshot().active_acp_requests, 1);
        let second = tokio::time::timeout(Duration::from_millis(20), runtime.acquire(&cancellation)).await;
        assert!(second.is_err(), "second request must remain queued");
        assert_eq!(runtime.telemetry_snapshot().acp_local_queue_depth, 0, "cancelled wait must leave the queue");

        drop(first);
        assert_eq!(runtime.telemetry_snapshot().active_acp_requests, 0);
        let _released = runtime.acquire(&cancellation).await.expect("released permit");
    }

    #[tokio::test]
    async fn telemetry_counts_a_waiting_request() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = SessionCancellation::default();
        let _first = runtime.acquire(&cancellation).await.expect("first permit");
        let queued_runtime = runtime.clone();
        let queued_cancellation = cancellation.clone();
        let queued = tokio::spawn(async move { queued_runtime.acquire(&queued_cancellation).await });
        tokio::task::yield_now().await;

        let snapshot = runtime.telemetry_snapshot();
        assert_eq!(snapshot.acp_local_queue_depth, 1);
        assert_eq!(snapshot.active_acp_requests, 1);
        assert_eq!(snapshot.configured_provider_limit, Some(1));
        assert_eq!(snapshot.circuit_breaker_state, "not_configured");

        cancellation.cancel();
        let error = queued.await.expect("queued task").expect_err("queue should be cancelled");
        assert!(matches!(error, ProviderAdmissionError::Cancelled));
        assert_eq!(runtime.telemetry_snapshot().acp_local_queue_depth, 0);
    }

    #[tokio::test]
    async fn queued_request_times_out() {
        let runtime = runtime(1, Duration::from_millis(10));
        let cancellation = SessionCancellation::default();
        let _first = runtime.acquire(&cancellation).await.expect("first permit");

        let error = runtime.acquire(&cancellation).await.expect_err("queue should time out");
        assert!(matches!(error, ProviderAdmissionError::QueueTimeout { limit: 1, .. }));
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_queued_request() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = SessionCancellation::default();
        let _first = runtime.acquire(&cancellation).await.expect("first permit");

        let queued_cancellation = cancellation.clone();
        let queued_runtime = runtime.clone();
        let queued = tokio::spawn(async move { queued_runtime.acquire(&queued_cancellation).await });
        tokio::task::yield_now().await;
        cancellation.cancel();

        let error = queued.await.expect("queued task").expect_err("queue should be cancelled");
        assert!(matches!(error, ProviderAdmissionError::Cancelled));
    }

    #[test]
    fn registry_maps_custom_retry_policy_to_provider() {
        let provider = CustomProviderConfig {
            name: "ArLi".to_string(),
            request_policy: CustomProviderRequestPolicyConfig {
                max_in_flight_requests: Some(3),
                max_retries: 4,
                retry_initial_backoff_ms: 250,
                retry_max_backoff_ms: 5_000,
                retry_jitter: false,
                ..CustomProviderRequestPolicyConfig::default()
            },
            ..CustomProviderConfig::default()
        };
        let runtime =
            ProviderRuntimeRegistry::new(&[provider], &TimeoutsConfig::default(), Arc::new(MetricsCollector::new()))
                .for_provider("arli");

        assert_eq!(runtime.limit, Some(3));
        assert!(runtime.semaphore.is_none(), "custom-provider capacity is enforced at the shared router boundary");
        assert_eq!(runtime.retry_policy.max_attempts, 5);
        assert_eq!(runtime.retry_policy.initial_delay, Duration::from_millis(250));
        assert_eq!(runtime.retry_policy.max_delay, Duration::from_secs(5));
        assert!(runtime.retry_policy.jitter.abs() < f64::EPSILON);
        assert_eq!(runtime.deadline_policy.connect, Some(Duration::from_secs(30)));
        assert_eq!(runtime.deadline_policy.first_token, Some(Duration::from_secs(180)));
        assert_eq!(runtime.deadline_policy.stream_idle, Some(Duration::from_secs(120)));
        assert_eq!(runtime.deadline_policy.total_generation, Some(Duration::from_secs(600)));
    }
}
