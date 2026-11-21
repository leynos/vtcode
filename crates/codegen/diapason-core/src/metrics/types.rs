//! Public metric record types, observer ports, and runtime cache kinds.
//!
//! These data shapes are the vocabulary the collector records and exports, so
//! they live together and are re-exported from the parent module to keep the
//! `diapason_core::metrics` paths stable.

use serde::{Deserialize, Serialize};

/// Frozen, bounded snapshot of the process-wide admission counters.
///
/// The values are copied out of the provider's counters in one read, so the
/// snapshot is *not* a consistent cross-counter view: `queue_depth` and
/// `in_flight` are current gauges while the remaining fields are cumulative
/// counters, and concurrent updates may move them between individual loads.
/// The snapshot is process-wide, not collector-scoped, and reading it neither
/// mutates nor resets the underlying counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderAdmissionSnapshot {
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

/// Consumer-owned port for sampling the process-wide provider admission counters.
///
/// `MetricsCollector` depends on this narrow port rather than on any provider
/// crate. Implementations live outside this module and are injected at the
/// composition boundary, which keeps the metrics module free of provider
/// dependencies and lets tests substitute a deterministic source.
///
/// Implementations must sample live counters on every call: the collector reads
/// the source at export/read time so a snapshot is never cached across reads.
pub trait ProviderAdmissionMetricsSource: std::fmt::Debug + Send + Sync {
    /// Read the current process-wide admission counters.
    fn snapshot(&self) -> ProviderAdmissionSnapshot;
}

/// Bounded aggregate metrics for model-provider request attempts.
///
/// Provider names are deliberately not labels here: user-defined provider
/// names would create unbounded Prometheus series cardinality.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderRequestMetrics {
    pub attempts_started: u64,
    pub attempts_succeeded: u64,
    pub attempts_failed: u64,
    pub attempts_cancelled: u64,
    pub retries_scheduled: u64,
    pub attempts_in_flight: u64,
    pub total_duration_ms: u64,
    pub first_output_samples: u64,
    pub total_time_to_first_output_ms: u64,
    pub custom_provider_queue_depth: u64,
    pub custom_provider_in_flight: u64,
    pub custom_provider_queue_waits: u64,
    pub custom_provider_queue_wait_ms: u64,
    pub custom_provider_queue_timeouts: u64,
}

/// Bounded aggregate outcomes and timings for MCP client/provider startup.
///
/// This snapshot intentionally contains no provider names or request payloads,
/// so the same fixed metrics are safe to expose through JSON and Prometheus.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStartupMetrics {
    pub client_attempts_started: u64,
    pub client_attempts_succeeded: u64,
    pub client_attempts_partial: u64,
    pub client_attempts_failed: u64,
    pub client_attempts_disabled: u64,
    pub client_attempts_cancelled: u64,
    pub client_attempts_in_flight: u64,
    pub provider_attempts_started: u64,
    pub provider_attempts_succeeded: u64,
    pub provider_attempts_failed: u64,
    pub provider_attempts_cancelled: u64,
    pub provider_attempts_in_flight: u64,
    pub provider_connection_failures: u64,
    pub provider_connection_timeouts: u64,
    pub provider_initialization_failures: u64,
    pub provider_initialization_timeouts: u64,
    pub provider_other_failures: u64,
    pub provider_timeouts: u64,
    pub active_providers: u64,
    pub total_client_duration_ms: u64,
    pub total_provider_duration_ms: u64,
}

/// Bounded aggregate metrics for task-lifecycle notification queues.
///
/// These values cover all active sessions and deliberately carry no session,
/// task, or payload labels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLifecycleNotificationQueueMetrics {
    pub queue_depth: u64,
    pub queue_dropped_full: u64,
    pub queue_dropped_closed: u64,
}

/// Bounded client startup outcomes without integration-specific provider data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceClientStartupOutcome {
    Succeeded,
    Partial,
    Failed,
    Disabled,
    Cancelled,
}

/// Bounded provider startup outcomes without integration-specific provider data.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServiceProviderStartupOutcome {
    Succeeded,
    ConnectionFailed,
    ConnectionTimedOut,
    InitializationFailed,
    InitializationTimedOut,
    OtherFailed,
    Cancelled,
}

/// Driven port for bounded service-client and provider startup metrics.
pub trait ServiceStartupMetricsObserver: Send + Sync {
    fn client_startup_started(&self);
    fn client_startup_finished(
        &self,
        outcome: ServiceClientStartupOutcome,
        previous_active_provider_count: u64,
        active_provider_count: u64,
        duration_ms: u64,
    );
    fn provider_startup_started(&self);
    fn provider_startup_finished(&self, outcome: ServiceProviderStartupOutcome, duration_ms: u64);
}

/// Bounded queue drop reasons for task-lifecycle notification adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskLifecycleNotificationQueueDrop {
    Full,
    Closed,
}

/// Driven port for tracking a bounded task-lifecycle notification queue.
pub trait TaskLifecycleNotificationQueueMetricsObserver: Send + Sync {
    fn queue_item_enqueued(&self);
    fn queue_item_dequeued(&self);
    fn queue_items_drained(&self, entries: usize);
    fn queue_item_dropped(&self, reason: TaskLifecycleNotificationQueueDrop) -> u64;
}

/// The kind of runtime cache whose bounded entry count is being observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCacheKind {
    Workspace,
}
