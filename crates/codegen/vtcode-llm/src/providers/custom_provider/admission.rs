//! Shared concurrency admission and aggregate metrics for custom-provider requests.
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vtcode_commons::llm::LLMErrorMetadata;
use vtcode_config::core::CustomProviderConfig;

use crate::provider::LLMError;

static SHARED_PROVIDER_SEMAPHORES: LazyLock<Mutex<HashMap<String, Weak<Semaphore>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ADMISSION_METRICS: LazyLock<AdmissionMetricCounters> = LazyLock::new(AdmissionMetricCounters::default);

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct CustomProviderAdmissionMetrics {
    pub queue_depth: u64,
    pub in_flight: u64,
    pub queue_waits: u64,
    pub queue_wait_ms: u64,
    pub queue_timeouts: u64,
}

#[derive(Default)]
struct AdmissionMetricCounters {
    queue_depth: AtomicU64,
    in_flight: AtomicU64,
    queue_waits: AtomicU64,
    queue_wait_ms: AtomicU64,
    queue_timeouts: AtomicU64,
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_add(amount)));
}

fn saturating_sub_one(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_sub(1)));
}

/// Samples the process-wide aggregate custom-provider admission counters.
///
/// `queue_depth` and `in_flight` are current counts; the remaining fields are
/// cumulative queue waits, wait time in milliseconds and queue timeouts. Each
/// field is loaded independently with relaxed ordering, so concurrent updates
/// can make the values reflect slightly different instants. Reading the
/// snapshot does not mutate or reset the counters.
pub fn custom_provider_admission_metrics() -> CustomProviderAdmissionMetrics {
    CustomProviderAdmissionMetrics {
        queue_depth: ADMISSION_METRICS.queue_depth.load(Ordering::Relaxed),
        in_flight: ADMISSION_METRICS.in_flight.load(Ordering::Relaxed),
        queue_waits: ADMISSION_METRICS.queue_waits.load(Ordering::Relaxed),
        queue_wait_ms: ADMISSION_METRICS.queue_wait_ms.load(Ordering::Relaxed),
        queue_timeouts: ADMISSION_METRICS.queue_timeouts.load(Ordering::Relaxed),
    }
}

struct AdmissionQueueWait {
    provider_name: Arc<str>,
    started_at: Instant,
    timed_out: bool,
}

impl AdmissionQueueWait {
    fn start(provider_name: Arc<str>) -> Self {
        saturating_add(&ADMISSION_METRICS.queue_depth, 1);
        Self {
            provider_name,
            started_at: Instant::now(),
            timed_out: false,
        }
    }
}

impl Drop for AdmissionQueueWait {
    fn drop(&mut self) {
        let waited = self.started_at.elapsed();
        let waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        saturating_sub_one(&ADMISSION_METRICS.queue_depth);
        saturating_add(&ADMISSION_METRICS.queue_waits, 1);
        saturating_add(&ADMISSION_METRICS.queue_wait_ms, waited_ms);
        if self.timed_out {
            saturating_add(&ADMISSION_METRICS.queue_timeouts, 1);
            tracing::warn!(
                provider = self.provider_name.as_ref(),
                queue_wait_ms = waited_ms,
                "Custom provider request admission timed out"
            );
        } else {
            tracing::debug!(
                provider = self.provider_name.as_ref(),
                queue_wait_ms = waited_ms,
                "Custom provider request admitted"
            );
        }
    }
}

#[derive(Debug)]
pub(super) struct CustomProviderAdmissionPermit {
    _permit: OwnedSemaphorePermit,
}

impl CustomProviderAdmissionPermit {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        saturating_add(&ADMISSION_METRICS.in_flight, 1);
        Self { _permit: permit }
    }
}

impl Drop for CustomProviderAdmissionPermit {
    fn drop(&mut self) {
        saturating_sub_one(&ADMISSION_METRICS.in_flight);
    }
}

#[derive(Clone)]
pub(super) struct CustomProviderAdmission {
    pub(super) provider_name: Arc<str>,
    pub(super) semaphore: Option<Arc<Semaphore>>,
    pub(super) queue_timeout: Duration,
}

impl CustomProviderAdmission {
    pub(super) fn new(config: &CustomProviderConfig, effective_base_url: &str) -> Self {
        let provider_name: Arc<str> = Arc::from(config.name.as_str());
        let semaphore = config.request_policy.max_in_flight_requests.map(|limit| {
            let identity = serde_json::to_vec(&(config, effective_base_url)).ok();
            match identity {
                Some(identity) => {
                    let key = vtcode_commons::utils::calculate_sha256(&identity);
                    let mut shared = SHARED_PROVIDER_SEMAPHORES
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    shared.retain(|_, semaphore| semaphore.strong_count() > 0);
                    if let Some(semaphore) = shared.get(&key).and_then(Weak::upgrade) {
                        semaphore
                    } else {
                        let semaphore = Arc::new(Semaphore::new(limit));
                        shared.insert(key, Arc::downgrade(&semaphore));
                        semaphore
                    }
                }
                None => {
                    tracing::warn!(
                        provider = %provider_name,
                        "Unable to identify custom provider configuration for shared request admission; using an isolated gate"
                    );
                    Arc::new(Semaphore::new(limit))
                }
            }
        });

        Self {
            provider_name,
            semaphore,
            queue_timeout: Duration::from_secs(config.request_policy.queue_timeout_seconds),
        }
    }

    pub(super) async fn acquire(&self) -> Result<Option<CustomProviderAdmissionPermit>, LLMError> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(None);
        };
        let mut queue_wait = AdmissionQueueWait::start(Arc::clone(&self.provider_name));
        match tokio::time::timeout(self.queue_timeout, Arc::clone(semaphore).acquire_owned()).await {
            Ok(Ok(permit)) => {
                drop(queue_wait);
                Ok(Some(CustomProviderAdmissionPermit::new(permit)))
            }
            Ok(Err(_closed)) => Err(LLMError::Provider {
                message: format!("custom provider '{}' request admission is closed", self.provider_name),
                metadata: Some(LLMErrorMetadata::new(
                    self.provider_name.as_ref(),
                    None,
                    Some("custom_provider_admission_closed".to_owned()),
                    None,
                    None,
                    None,
                    None,
                )),
            }),
            Err(_elapsed) => {
                queue_wait.timed_out = true;
                Err(LLMError::Provider {
                    message: format!(
                        "custom provider '{}' remained at its configured request limit for {:?}",
                        self.provider_name, self.queue_timeout,
                    ),
                    metadata: Some(LLMErrorMetadata::new(
                        self.provider_name.as_ref(),
                        None,
                        Some("custom_provider_admission_timeout".to_owned()),
                        None,
                        None,
                        None,
                        None,
                    )),
                })
            }
        }
    }
}
