//! Shared concurrency admission and aggregate metrics for custom-provider requests.
use crate::provider_config_types::RequestAdmissionClass;
use diapason_commons::llm::LLMErrorMetadata;
use diapason_config::core::CustomProviderConfig;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::Duration;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::provider::LLMError;

static SHARED_PROVIDER_ADMISSIONS: LazyLock<Mutex<HashMap<String, Weak<SharedProviderAdmission>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ADMISSION_METRICS: LazyLock<AdmissionMetricCounters> = LazyLock::new(AdmissionMetricCounters::default);

#[derive(Debug)]
pub(super) struct SharedProviderAdmission {
    semaphore: Arc<Semaphore>,
    child_limit: usize,
    child_in_flight: AtomicUsize,
    child_notify: Notify,
}

impl SharedProviderAdmission {
    fn new(limit: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(limit)),
            child_limit: limit.saturating_sub(1),
            child_in_flight: AtomicUsize::new(0),
            child_notify: Notify::new(),
        }
    }

    fn try_acquire_child_slot(&self) -> bool {
        let mut current = self.child_in_flight.load(Ordering::Acquire);
        loop {
            if current >= self.child_limit {
                return false;
            }
            match self
                .child_in_flight
                .compare_exchange_weak(current, current + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release_child_slot(&self) {
        let mut current = self.child_in_flight.load(Ordering::Acquire);
        loop {
            let next = current.saturating_sub(1);
            match self
                .child_in_flight
                .compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        self.child_notify.notify_waiters();
    }
}

#[derive(Debug)]
struct ChildSlotReservation {
    shared: Arc<SharedProviderAdmission>,
}

impl Drop for ChildSlotReservation {
    fn drop(&mut self) {
        self.shared.release_child_slot();
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum AdmissionWaitError {
    TimedOut,
    Closed,
}

fn admission_deadline(timeout: Duration) -> Option<Instant> {
    // A duration outside Instant's range has no representable expiration. Treat
    // it as unbounded rather than panicking or timing out earlier than asked.
    Instant::now().checked_add(timeout)
}

fn deadline_expired(deadline: Option<Instant>) -> bool {
    deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

async fn acquire_provider_permit(
    semaphore: Arc<Semaphore>,
    deadline: Option<Instant>,
) -> Result<OwnedSemaphorePermit, AdmissionWaitError> {
    if deadline_expired(deadline) {
        return Err(AdmissionWaitError::TimedOut);
    }

    let acquire = semaphore.acquire_owned();
    match deadline {
        Some(deadline) => tokio::time::timeout_at(deadline, acquire)
            .await
            .map_err(|_elapsed| AdmissionWaitError::TimedOut)?
            .map_err(|_closed| AdmissionWaitError::Closed),
        None => acquire.await.map_err(|_closed| AdmissionWaitError::Closed),
    }
}

async fn reserve_child_slot(
    shared: Arc<SharedProviderAdmission>,
    deadline: Option<Instant>,
) -> Result<ChildSlotReservation, AdmissionWaitError> {
    loop {
        if deadline_expired(deadline) {
            return Err(AdmissionWaitError::TimedOut);
        }

        let mut child_released = Box::pin(shared.child_notify.notified());
        // Register before checking the counter so a release between the check
        // and the wait cannot be lost.
        child_released.as_mut().enable();
        if shared.try_acquire_child_slot() {
            drop(child_released);
            return Ok(ChildSlotReservation { shared });
        }

        match deadline {
            Some(deadline) => tokio::time::timeout_at(deadline, child_released.as_mut())
                .await
                .map_err(|_elapsed| AdmissionWaitError::TimedOut)?,
            None => child_released.as_mut().await,
        }
    }
}

/// A process-wide snapshot of custom-provider request admission activity.
///
/// Current counts and cumulative counters are loaded independently, so a
/// snapshot may combine values from slightly different instants.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct CustomProviderAdmissionMetrics {
    /// Requests currently waiting for a provider concurrency slot.
    pub queue_depth: u64,
    /// Requests currently holding a provider concurrency slot.
    pub in_flight: u64,
    /// Cumulative number of requests that waited for admission.
    pub queue_waits: u64,
    /// Cumulative admission wait time in milliseconds.
    pub queue_wait_ms: u64,
    /// Cumulative number of requests that timed out while waiting for admission.
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
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_add(amount);
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn saturating_sub_one(counter: &AtomicU64) {
    let mut current = counter.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(1);
        match counter.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
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
    child_admission: Option<ChildSlotReservation>,
}

impl CustomProviderAdmissionPermit {
    fn new(permit: OwnedSemaphorePermit, child_admission: Option<ChildSlotReservation>) -> Self {
        saturating_add(&ADMISSION_METRICS.in_flight, 1);
        Self { _permit: permit, child_admission }
    }
}

impl Drop for CustomProviderAdmissionPermit {
    fn drop(&mut self) {
        saturating_sub_one(&ADMISSION_METRICS.in_flight);
        drop(self.child_admission.take());
    }
}

#[derive(Clone)]
pub(super) struct CustomProviderAdmission {
    pub(super) provider_name: Arc<str>,
    pub(super) semaphore: Option<Arc<Semaphore>>,
    pub(super) queue_timeout: Duration,
    pub(super) shared: Option<Arc<SharedProviderAdmission>>,
    pub(super) request_admission_class: RequestAdmissionClass,
}

impl CustomProviderAdmission {
    pub(super) fn new(
        config: &CustomProviderConfig,
        effective_base_url: &str,
        request_admission_class: RequestAdmissionClass,
    ) -> Self {
        let provider_name: Arc<str> = Arc::from(config.name.as_str());
        let shared = config.request_policy.max_in_flight_requests.map(|limit| {
            let identity = serde_json::to_vec(&(config, effective_base_url)).ok();
            match identity {
                Some(identity) => {
                    let key = diapason_commons::utils::calculate_sha256(&identity);
                    let mut shared = SHARED_PROVIDER_ADMISSIONS
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    shared.retain(|_, admission| admission.strong_count() > 0);
                    if let Some(admission) = shared.get(&key).and_then(Weak::upgrade) {
                        admission
                    } else {
                        let admission = Arc::new(SharedProviderAdmission::new(limit));
                        shared.insert(key, Arc::downgrade(&admission));
                        admission
                    }
                }
                None => {
                    tracing::warn!(
                        provider = %provider_name,
                        "Unable to identify custom provider configuration for shared request admission; using an isolated gate"
                    );
                    Arc::new(SharedProviderAdmission::new(limit))
                }
            }
        });

        Self {
            provider_name,
            semaphore: shared.as_ref().map(|admission| Arc::clone(&admission.semaphore)),
            queue_timeout: Duration::from_secs(config.request_policy.queue_timeout_seconds),
            shared,
            request_admission_class,
        }
    }

    pub(super) async fn acquire(&self) -> Result<Option<CustomProviderAdmissionPermit>, LLMError> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(None);
        };
        let mut queue_wait = AdmissionQueueWait::start(Arc::clone(&self.provider_name));
        let deadline = admission_deadline(self.queue_timeout);

        let child_admission = if self.request_admission_class == RequestAdmissionClass::Child {
            let Some(shared) = self.shared.as_ref().cloned() else {
                return Err(LLMError::Provider {
                    message: format!("custom provider '{}' has invalid subagent admission state", self.provider_name),
                    metadata: Some(LLMErrorMetadata::new(
                        self.provider_name.as_ref(),
                        None,
                        Some("custom_provider_admission_invalid_state".to_owned()),
                        None,
                        None,
                        None,
                        None,
                    )),
                });
            };
            match reserve_child_slot(shared, deadline).await {
                Ok(reservation) => Some(reservation),
                Err(AdmissionWaitError::TimedOut) => {
                    queue_wait.timed_out = true;
                    return Err(self.timeout_error());
                }
                Err(AdmissionWaitError::Closed) => return Err(self.closed_error()),
            }
        } else {
            None
        };

        let permit = match acquire_provider_permit(Arc::clone(semaphore), deadline).await {
            Ok(permit) => permit,
            Err(AdmissionWaitError::Closed) => return Err(self.closed_error()),
            Err(AdmissionWaitError::TimedOut) => {
                queue_wait.timed_out = true;
                return Err(self.timeout_error());
            }
        };

        drop(queue_wait);
        Ok(Some(CustomProviderAdmissionPermit::new(permit, child_admission)))
    }

    fn closed_error(&self) -> LLMError {
        LLMError::Provider {
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
        }
    }

    fn timeout_error(&self) -> LLMError {
        LLMError::Provider {
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
        }
    }
}
