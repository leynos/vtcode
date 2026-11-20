//! Collects execution and tool metrics and exposes the metric record types used by core.
// Metrics collection and observability for MCP execution system
//
// Tracks performance, effectiveness, and security across all execution steps:
// - Tool discovery (hit rate, response time)
// - Code execution (duration, success rate, memory)
// - SDK generation (overhead, caching)
// - Data filtering (reduction ratio, token savings)
// - Skill usage (adoption, reuse patterns)
// - PII detection (pattern matches, audit trail)

use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

// The Lody ACP extension family lives in its own module to keep this file
// navigable; its public items are re-exported below.
mod acp_metrics;

// Recording, querying, rendering, and observer adapters live in sibling
// modules; every public item is re-exported below so `crate::metrics::*`
// paths stay stable.
mod collector_core;

mod collector_recording;

mod observers;

mod prometheus;

mod prometheus_helpers;

mod types;

pub mod discovery_metrics;

pub mod execution_metrics;

pub mod filtering_metrics;

pub mod patch_safety_metrics;

pub mod planning_metrics;

pub mod sdk_metrics;

pub mod security_metrics;

pub mod skill_metrics;

pub mod tool_recovery_metrics;

pub use acp_metrics::{AcpExtensionMetrics, AcpManagementGuard};
pub use discovery_metrics::DiscoveryMetrics;
pub use execution_metrics::ExecutionMetrics;
pub use filtering_metrics::FilteringMetrics;
pub use patch_safety_metrics::{
    PatchCertificationOutcome, PatchFailureGuardOutcome, PatchNoOpOutcome, PatchPreconditionOutcome, PatchSafetyMetrics,
};
pub use planning_metrics::{PlanValidationRejectionReason, PlanningValidationMetrics};
pub use sdk_metrics::SdkMetrics;
pub use security_metrics::SecurityMetrics;
pub use skill_metrics::SkillMetrics;
pub use tool_recovery_metrics::{ToolRecoveryEvent, ToolRecoveryMetrics};
pub use types::{
    ProviderAdmissionMetricsSource, ProviderAdmissionSnapshot, ProviderRequestMetrics, RuntimeCacheKind,
    ServiceClientStartupOutcome, ServiceProviderStartupOutcome, ServiceStartupMetrics, ServiceStartupMetricsObserver,
    TaskLifecycleNotificationQueueDrop, TaskLifecycleNotificationQueueMetrics,
    TaskLifecycleNotificationQueueMetricsObserver,
};

#[cfg(test)]
pub use test_support::ZeroProviderAdmissionMetricsSource;

#[cfg(test)]
mod test_support;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod tests_provider_admission;

/// Central metrics collector for all MCP execution activities
#[derive(Debug, Clone)]
pub struct MetricsCollector {
    discovery: Arc<Mutex<DiscoveryMetrics>>,
    execution: Arc<Mutex<ExecutionMetrics>>,
    sdk: Arc<Mutex<SdkMetrics>>,
    filtering: Arc<Mutex<FilteringMetrics>>,
    patch_safety: Arc<Mutex<PatchSafetyMetrics>>,
    tool_recovery: Arc<Mutex<ToolRecoveryMetrics>>,
    planning: Arc<Mutex<PlanningValidationMetrics>>,
    skills: Arc<Mutex<SkillMetrics>>,
    security: Arc<Mutex<SecurityMetrics>>,
    provider_requests: Arc<Mutex<ProviderRequestMetrics>>,
    service_startup: Arc<Mutex<ServiceStartupMetrics>>,
    task_lifecycle_notification_queue: Arc<Mutex<TaskLifecycleNotificationQueueMetrics>>,
    acp_extension: Arc<Mutex<acp_metrics::AcpExtensionState>>,
    workspace_runtime_cache_entries: Arc<AtomicU64>,
    admission_metrics_source: Arc<dyn ProviderAdmissionMetricsSource>,
    start_time: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsSummary {
    pub timestamp: DateTime<Utc>,
    pub session_duration_ms: u64,
    pub discovery: DiscoveryMetrics,
    pub execution: ExecutionMetrics,
    pub sdk: SdkMetrics,
    pub filtering: FilteringMetrics,
    #[serde(default)]
    pub patch_safety: PatchSafetyMetrics,
    #[serde(default)]
    pub tool_recovery: ToolRecoveryMetrics,
    pub planning: PlanningValidationMetrics,
    pub skills: SkillMetrics,
    pub security: SecurityMetrics,
    pub provider_requests: ProviderRequestMetrics,
    #[serde(default)]
    pub mcp_startup: ServiceStartupMetrics,
    #[serde(default)]
    pub task_lifecycle_notification_queue: TaskLifecycleNotificationQueueMetrics,
    #[serde(default)]
    pub workspace_runtime_cache_entries: u64,
}

struct PrometheusMetricsSnapshot {
    discovery: DiscoveryMetrics,
    execution: ExecutionMetrics,
    filtering: FilteringMetrics,
    patch_safety: PatchSafetyMetrics,
    tool_recovery: ToolRecoveryMetrics,
    planning: PlanningValidationMetrics,
    skills: SkillMetrics,
    security: SecurityMetrics,
    provider_requests: ProviderRequestMetrics,
    service_startup: ServiceStartupMetrics,
    task_lifecycle_notification_queue: TaskLifecycleNotificationQueueMetrics,
    acp_extension: AcpExtensionMetrics,
    workspace_runtime_cache_entries: u64,
}
