use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vtcode_config::TimeoutsConfig;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
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
    ) -> Self {
        let provider_name = provider_name.into();
        let semaphore = config.max_in_flight_requests.map(|limit| Arc::new(Semaphore::new(limit)));
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

    pub(crate) fn apply_http_timeouts(&self, timeouts: &mut TimeoutsConfig) {
        timeouts.connect_timeout_seconds = self.deadline_policy.connect.map_or(0, |duration| duration.as_secs());
        let total = self.deadline_policy.total_generation.map_or(0, |duration| duration.as_secs());
        timeouts.default_ceiling_seconds = total;
        timeouts.streaming_ceiling_seconds = total;
    }

    pub(crate) async fn acquire(
        &self,
        cancellation: &SessionCancellation,
    ) -> Result<ProviderPermit, ProviderAdmissionError> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(ProviderPermit { _permit: None });
        };

        let acquire = semaphore.clone().acquire_owned();
        tokio::select! {
            () = cancellation.cancelled() => Err(ProviderAdmissionError::Cancelled),
            result = tokio::time::timeout(self.queue_timeout, acquire) => {
                match result {
                    Ok(Ok(permit)) => Ok(ProviderPermit { _permit: Some(permit) }),
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
    pub(crate) fn new(custom_providers: &[CustomProviderConfig], timeouts: &TimeoutsConfig) -> Self {
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
                    ),
                )
            })
            .collect();

        Self {
            default_runtime: ProviderRequestRuntime::new(
                "default",
                &default_config,
                ProviderDeadlinePolicy::from_timeouts(timeouts),
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
        }
    }

    #[tokio::test]
    async fn permit_limit_blocks_until_the_active_request_finishes() {
        let runtime = runtime(1, Duration::from_secs(1));
        let cancellation = SessionCancellation::default();
        let first = runtime.acquire(&cancellation).await.expect("first permit");
        let second = tokio::time::timeout(Duration::from_millis(20), runtime.acquire(&cancellation)).await;
        assert!(second.is_err(), "second request must remain queued");

        drop(first);
        let _released = runtime.acquire(&cancellation).await.expect("released permit");
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
        let runtime = ProviderRuntimeRegistry::new(&[provider], &TimeoutsConfig::default()).for_provider("arli");

        assert_eq!(runtime.limit, Some(3));
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
