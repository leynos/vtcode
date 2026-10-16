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
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub mod discovery_metrics;
pub mod execution_metrics;
pub mod filtering_metrics;
pub mod planning_metrics;
pub mod sdk_metrics;
pub mod security_metrics;
pub mod skill_metrics;

pub use discovery_metrics::DiscoveryMetrics;
pub use execution_metrics::ExecutionMetrics;
pub use filtering_metrics::FilteringMetrics;
pub use planning_metrics::{PlanValidationRejectionReason, PlanningValidationMetrics};
pub use sdk_metrics::SdkMetrics;
pub use security_metrics::SecurityMetrics;
pub use skill_metrics::SkillMetrics;

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

/// Bounded aggregate metrics for ACP task-lifecycle notification queues.
///
/// These values cover all active sessions and deliberately carry no session,
/// task, or payload labels.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskLifecycleQueueMetrics {
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

/// Bounded queue events used by task-lifecycle notification adapters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskLifecycleQueueDrop {
    Full,
    Closed,
}

/// Driven port for tracking a bounded task-lifecycle notification queue.
pub trait TaskLifecycleQueueMetricsObserver: Send + Sync {
    fn queue_item_enqueued(&self);
    fn queue_item_dequeued(&self);
    fn queue_items_drained(&self, entries: usize);
    fn queue_item_dropped(&self, reason: TaskLifecycleQueueDrop) -> u64;
}

/// The kind of runtime cache whose bounded entry count is being observed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RuntimeCacheKind {
    Workspace,
}

/// Central metrics collector for all MCP execution activities
#[derive(Debug, Clone)]
pub struct MetricsCollector {
    discovery: Arc<Mutex<DiscoveryMetrics>>,
    execution: Arc<Mutex<ExecutionMetrics>>,
    sdk: Arc<Mutex<SdkMetrics>>,
    filtering: Arc<Mutex<FilteringMetrics>>,
    planning: Arc<Mutex<PlanningValidationMetrics>>,
    skills: Arc<Mutex<SkillMetrics>>,
    security: Arc<Mutex<SecurityMetrics>>,
    provider_requests: Arc<Mutex<ProviderRequestMetrics>>,
    service_startup: Arc<Mutex<ServiceStartupMetrics>>,
    task_lifecycle_queue: Arc<Mutex<TaskLifecycleQueueMetrics>>,
    workspace_runtime_cache_entries: Arc<AtomicU64>,
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
    pub planning: PlanningValidationMetrics,
    pub skills: SkillMetrics,
    pub security: SecurityMetrics,
    pub provider_requests: ProviderRequestMetrics,
    #[serde(default)]
    pub mcp_startup: ServiceStartupMetrics,
    #[serde(default)]
    pub acp_task_lifecycle: TaskLifecycleQueueMetrics,
    #[serde(default)]
    pub acp_workspace_runtime_cache_entries: u64,
}

impl MetricsCollector {
    /// Create a new metrics collector
    pub fn new() -> Self {
        Self {
            discovery: Arc::new(Mutex::new(DiscoveryMetrics::new())),
            execution: Arc::new(Mutex::new(ExecutionMetrics::new())),
            sdk: Arc::new(Mutex::new(SdkMetrics::new())),
            filtering: Arc::new(Mutex::new(FilteringMetrics::new())),
            planning: Arc::new(Mutex::new(PlanningValidationMetrics::default())),
            skills: Arc::new(Mutex::new(SkillMetrics::new())),
            security: Arc::new(Mutex::new(SecurityMetrics::new())),
            provider_requests: Arc::new(Mutex::new(ProviderRequestMetrics::default())),
            service_startup: Arc::new(Mutex::new(ServiceStartupMetrics::default())),
            task_lifecycle_queue: Arc::new(Mutex::new(TaskLifecycleQueueMetrics::default())),
            workspace_runtime_cache_entries: Arc::new(AtomicU64::new(0)),
            start_time: Instant::now(),
        }
    }

    /// Record a provider request attempt and update its active gauge.
    pub fn record_provider_attempt_started(&self) {
        let mut metrics = self.provider_requests.lock();
        metrics.attempts_started = metrics.attempts_started.saturating_add(1);
        metrics.attempts_in_flight = metrics.attempts_in_flight.saturating_add(1);
    }

    /// Record a completed provider request attempt and its bounded timing totals.
    pub fn record_provider_attempt_succeeded(&self, duration_ms: u64, first_output_ms: Option<u64>) {
        let mut metrics = self.provider_requests.lock();
        metrics.attempts_succeeded = metrics.attempts_succeeded.saturating_add(1);
        metrics.total_duration_ms = metrics.total_duration_ms.saturating_add(duration_ms);
        if let Some(first_output_ms) = first_output_ms {
            metrics.first_output_samples = metrics.first_output_samples.saturating_add(1);
            metrics.total_time_to_first_output_ms =
                metrics.total_time_to_first_output_ms.saturating_add(first_output_ms);
        }
    }

    /// Record a failed or cancelled provider request attempt and its timing total.
    pub fn record_provider_attempt_failed(&self, duration_ms: u64, cancelled: bool, first_output_ms: Option<u64>) {
        let mut metrics = self.provider_requests.lock();
        if cancelled {
            metrics.attempts_cancelled = metrics.attempts_cancelled.saturating_add(1);
        } else {
            metrics.attempts_failed = metrics.attempts_failed.saturating_add(1);
        }
        metrics.total_duration_ms = metrics.total_duration_ms.saturating_add(duration_ms);
        if let Some(first_output_ms) = first_output_ms {
            metrics.first_output_samples = metrics.first_output_samples.saturating_add(1);
            metrics.total_time_to_first_output_ms =
                metrics.total_time_to_first_output_ms.saturating_add(first_output_ms);
        }
    }

    /// Record a provider retry scheduled after a retryable failure.
    pub fn record_provider_retry_scheduled(&self) {
        let mut metrics = self.provider_requests.lock();
        metrics.retries_scheduled = metrics.retries_scheduled.saturating_add(1);
    }

    /// Mark a provider request attempt as no longer active.
    pub fn record_provider_attempt_finished(&self) {
        let mut metrics = self.provider_requests.lock();
        metrics.attempts_in_flight = metrics.attempts_in_flight.saturating_sub(1);
    }

    /// Get the bounded aggregate provider-request metrics snapshot.
    ///
    /// The `custom_provider_*` fields are sampled from process-wide admission
    /// counters shared across collectors, not scoped to this instance. Each
    /// counter is loaded independently with relaxed ordering.
    pub fn get_provider_request_metrics(&self) -> ProviderRequestMetrics {
        let admission = vtcode_llm::providers::custom_provider_admission_metrics();
        let mut metrics = self.provider_requests.lock().clone();
        metrics.custom_provider_queue_depth = admission.queue_depth;
        metrics.custom_provider_in_flight = admission.in_flight;
        metrics.custom_provider_queue_waits = admission.queue_waits;
        metrics.custom_provider_queue_wait_ms = admission.queue_wait_ms;
        metrics.custom_provider_queue_timeouts = admission.queue_timeouts;
        metrics
    }

    /// Get the bounded aggregate MCP startup metrics snapshot.
    pub fn get_service_startup_metrics(&self) -> ServiceStartupMetrics {
        self.service_startup.lock().clone()
    }

    /// Get aggregate measurements for the task-lifecycle notification queue.
    pub fn get_task_lifecycle_queue_metrics(&self) -> TaskLifecycleQueueMetrics {
        self.task_lifecycle_queue.lock().clone()
    }

    /// Set the current number of cached entries for a runtime cache kind.
    pub fn set_runtime_cache_entries(&self, kind: RuntimeCacheKind, entries: usize) {
        match kind {
            RuntimeCacheKind::Workspace => {}
        }
        let entries = u64::try_from(entries).unwrap_or(u64::MAX);
        self.workspace_runtime_cache_entries.store(entries, Ordering::Relaxed);
    }

    /// Get the current number of entries in the workspace runtime cache.
    pub fn get_runtime_cache_entries(&self, kind: RuntimeCacheKind) -> u64 {
        match kind {
            RuntimeCacheKind::Workspace => self.workspace_runtime_cache_entries.load(Ordering::Relaxed),
        }
    }

    // ========== Discovery Metrics ==========

    /// Record a tool discovery query
    pub fn record_discovery_query(&self, keyword: String, result_count: u64, response_time_ms: u64) {
        let mut metrics = self.discovery.lock();
        metrics.record_query(keyword, result_count, response_time_ms);
    }

    /// Record a failed discovery query
    pub fn record_discovery_failure(&self, keyword: String) {
        let mut metrics = self.discovery.lock();
        metrics.record_failure(keyword);
    }

    /// Record a discovery cache hit
    pub fn record_discovery_cache_hit(&self) {
        let mut metrics = self.discovery.lock();
        metrics.record_cache_hit();
    }

    // ========== Execution Metrics ==========

    /// Record the start of a code execution
    pub fn record_execution_start(&self, language: String) {
        let mut metrics = self.execution.lock();
        metrics.record_start(language);
    }

    /// Record successful execution completion
    pub fn record_execution_complete(&self, language: String, duration_ms: u64, memory_mb: u64) {
        let mut metrics = self.execution.lock();
        metrics.record_complete(language, duration_ms, memory_mb, true);
    }

    /// Record failed execution
    pub fn record_execution_failure(&self, language: String, duration_ms: u64) {
        let mut metrics = self.execution.lock();
        metrics.record_failure(language, duration_ms);
    }

    /// Record execution timeout
    pub fn record_execution_timeout(&self, language: String, duration_ms: u64) {
        let mut metrics = self.execution.lock();
        metrics.record_timeout(language, duration_ms);
    }

    /// Record a retry attempt for execution-related workflows.
    pub fn record_retry_attempt(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_retry_attempt();
    }

    /// Record a successful retry that eventually recovered.
    pub fn record_retry_success(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_retry_success();
    }

    /// Record an execution that exhausted all retry attempts.
    pub fn record_retry_exhausted(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_retry_exhausted();
    }

    /// Record that a circuit breaker entered the open state.
    pub fn record_circuit_open(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_circuit_open();
    }

    /// Record that a circuit breaker transitioned to half-open.
    pub fn record_half_open(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_half_open();
    }

    /// Record a denied request caused by an open circuit breaker.
    pub fn record_breaker_denial(&self) {
        let mut metrics = self.execution.lock();
        metrics.record_breaker_denial();
    }

    // ========== Circuit Breaker Helper Methods ==========

    /// Record circuit breaker metrics for an optional collector.
    ///
    /// This helper method is used by both `McpCircuitBreaker` and `ToolCircuitBreaker`
    /// to avoid code duplication.
    pub fn record_circuit_breaker_metrics(
        metrics: &Option<Arc<MetricsCollector>>,
        half_open: bool,
        denial: bool,
        circuit_open: bool,
    ) {
        if let Some(metrics) = metrics {
            if half_open {
                metrics.record_half_open();
            }
            if denial {
                metrics.record_breaker_denial();
            }
            if circuit_open {
                metrics.record_circuit_open();
            }
        }
    }

    /// Record result size for filtering calculation
    pub fn record_result_size(&self, size_bytes: usize) {
        let mut metrics = self.execution.lock();
        metrics.record_result_size(size_bytes);
    }

    // ========== SDK Metrics ==========

    /// Record SDK generation
    pub fn record_sdk_generation(&self, generation_time_ms: u64, tools_count: u64) {
        let mut metrics = self.sdk.lock();
        metrics.record_generation(generation_time_ms, tools_count);
    }

    /// Record SDK cache utilization
    pub fn record_sdk_cache_hit(&self) {
        let mut metrics = self.sdk.lock();
        metrics.record_cache_hit();
    }

    /// Record the estimated token overhead of the tool definitions sent in a
    /// request, accumulated across the session for transparency reporting.
    pub fn record_sdk_tool_definition_tokens(&self, tokens: u64) {
        let mut metrics = self.sdk.lock();
        metrics.record_tool_definition_tokens(tokens);
    }

    // ========== Filtering Metrics ==========

    /// Record a filtering operation
    pub fn record_filtering_operation(
        &self,
        operation_type: String,
        input_size: u64,
        output_size: u64,
        duration_ms: u64,
    ) {
        let mut metrics = self.filtering.lock();
        metrics.record_operation(operation_type, input_size, output_size, duration_ms);
    }

    /// Record a bounded plan validation rejection category.
    pub fn record_plan_validation_rejection(&self, reason: PlanValidationRejectionReason) {
        self.planning.lock().record_rejection(reason);
    }

    // ========== Skill Metrics ==========

    /// Record skill execution
    pub fn record_skill_execution(&self, skill_name: String, duration_ms: u64, success: bool) {
        let mut metrics = self.skills.lock();
        metrics.record_execution(skill_name, duration_ms, success);
    }

    /// Record skill creation
    pub fn record_skill_created(&self, skill_name: String, language: String) {
        let mut metrics = self.skills.lock();
        metrics.record_created(skill_name, language);
    }

    /// Record skill deletion
    pub fn record_skill_deleted(&self, skill_name: String) {
        let mut metrics = self.skills.lock();
        metrics.record_deleted(skill_name);
    }

    // ========== Security Metrics ==========

    /// Record PII pattern detection
    pub fn record_pii_detection(&self, pattern_type: String) {
        let mut metrics = self.security.lock();
        metrics.record_detection(pattern_type);
    }

    /// Record tokenization
    pub fn record_pii_tokenization(&self, token_count: usize) {
        let mut metrics = self.security.lock();
        metrics.record_tokenization(token_count);
    }

    /// Record audit event
    pub fn record_audit_event(&self, event_type: String, severity: String) {
        let mut metrics = self.security.lock();
        metrics.record_audit_event(event_type, severity);
    }

    // ========== Queries ==========

    /// Get current discovery metrics snapshot
    pub fn get_discovery_metrics(&self) -> DiscoveryMetrics {
        self.discovery.lock().clone()
    }

    /// Get current execution metrics snapshot
    pub fn get_execution_metrics(&self) -> ExecutionMetrics {
        self.execution.lock().clone()
    }

    /// Get current SDK metrics snapshot
    pub fn get_sdk_metrics(&self) -> SdkMetrics {
        self.sdk.lock().clone()
    }

    /// Get current filtering metrics snapshot
    pub fn get_filtering_metrics(&self) -> FilteringMetrics {
        self.filtering.lock().clone()
    }

    /// Get the current planning validation metrics snapshot.
    pub fn get_planning_metrics(&self) -> PlanningValidationMetrics {
        self.planning.lock().clone()
    }

    /// Get current skill metrics snapshot
    pub fn get_skill_metrics(&self) -> SkillMetrics {
        self.skills.lock().clone()
    }

    /// Get current security metrics snapshot
    pub fn get_security_metrics(&self) -> SecurityMetrics {
        self.security.lock().clone()
    }

    /// Get comprehensive summary of all metrics
    pub fn get_summary(&self) -> MetricsSummary {
        MetricsSummary {
            timestamp: Utc::now(),
            session_duration_ms: self.start_time.elapsed().as_millis().min(u64::MAX as u128) as u64,
            discovery: self.get_discovery_metrics(),
            execution: self.get_execution_metrics(),
            sdk: self.get_sdk_metrics(),
            filtering: self.get_filtering_metrics(),
            planning: self.get_planning_metrics(),
            skills: self.get_skill_metrics(),
            security: self.get_security_metrics(),
            provider_requests: self.get_provider_request_metrics(),
            mcp_startup: self.get_service_startup_metrics(),
            acp_task_lifecycle: self.get_task_lifecycle_queue_metrics(),
            acp_workspace_runtime_cache_entries: self.get_runtime_cache_entries(RuntimeCacheKind::Workspace),
        }
    }

    // ========== Export ==========

    /// Export metrics as JSON
    pub fn export_json(&self) -> anyhow::Result<serde_json::Value> {
        let summary = self.get_summary();
        Ok(serde_json::to_value(summary)?)
    }

    /// Export metrics in Prometheus format
    pub fn export_prometheus(&self) -> String {
        PrometheusMetricsSnapshot::collect(self).render()
    }
}

struct PrometheusMetricsSnapshot {
    discovery: DiscoveryMetrics,
    execution: ExecutionMetrics,
    filtering: FilteringMetrics,
    planning: PlanningValidationMetrics,
    skills: SkillMetrics,
    security: SecurityMetrics,
    provider_requests: ProviderRequestMetrics,
    service_startup: ServiceStartupMetrics,
    acp_task_lifecycle: TaskLifecycleQueueMetrics,
    workspace_runtime_cache_entries: u64,
}

impl PrometheusMetricsSnapshot {
    fn collect(collector: &MetricsCollector) -> Self {
        Self {
            discovery: collector.get_discovery_metrics(),
            execution: collector.get_execution_metrics(),
            filtering: collector.get_filtering_metrics(),
            planning: collector.get_planning_metrics(),
            skills: collector.get_skill_metrics(),
            security: collector.get_security_metrics(),
            provider_requests: collector.get_provider_request_metrics(),
            service_startup: collector.get_service_startup_metrics(),
            acp_task_lifecycle: collector.get_task_lifecycle_queue_metrics(),
            workspace_runtime_cache_entries: collector.get_runtime_cache_entries(RuntimeCacheKind::Workspace),
        }
    }

    fn render(&self) -> String {
        let mut output = String::new();
        self.append_discovery(&mut output);
        self.append_execution(&mut output);
        self.append_filtering(&mut output);
        self.append_skills(&mut output);
        self.append_pii_detections(&mut output);
        self.append_provider_requests(&mut output);
        self.append_service_startup(&mut output);
        self.append_acp_task_lifecycle(&mut output);
        self.append_acp_workspace_runtime_cache(&mut output);
        self.append_tokens_created(&mut output);
        output
    }

    fn append_discovery(&self, output: &mut String) {
        // Discovery metrics
        let _ = write!(
            output,
            "# HELP vtcode_discovery_queries_total Total tool discovery queries\n\
             # TYPE vtcode_discovery_queries_total counter\n\
             vtcode_discovery_queries_total {}\n\n",
            self.discovery.total_queries
        );

        let _ = write!(
            output,
            "# HELP vtcode_discovery_hit_rate Hit rate of discovery queries\n\
             # TYPE vtcode_discovery_hit_rate gauge\n\
             vtcode_discovery_hit_rate {}\n\n",
            self.discovery.hit_rate()
        );
    }

    fn append_execution(&self, output: &mut String) {
        // Execution metrics
        let _ = write!(
            output,
            "# HELP vtcode_execution_total Total code executions\n\
             # TYPE vtcode_execution_total counter\n\
             vtcode_execution_total {}\n\n",
            self.execution.total_executions
        );

        let _ = write!(
            output,
            "# HELP vtcode_execution_duration_ms Code execution average duration\n\
             # TYPE vtcode_execution_duration_ms gauge\n\
             vtcode_execution_duration_ms {}\n\n",
            self.execution.avg_duration_ms()
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_attempts_total Total retry attempts\n\
             # TYPE vtcode_retry_attempts_total counter\n\
             vtcode_retry_attempts_total {}\n\n",
            self.execution.retry_attempts
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_successes_total Total retries that later succeeded\n\
             # TYPE vtcode_retry_successes_total counter\n\
             vtcode_retry_successes_total {}\n\n",
            self.execution.retry_successes
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_exhausted_total Total operations that exhausted retries\n\
             # TYPE vtcode_retry_exhausted_total counter\n\
             vtcode_retry_exhausted_total {}\n\n",
            self.execution.retry_exhausted
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_open_total Total circuit breaker open transitions\n\
             # TYPE vtcode_circuit_open_total counter\n\
             vtcode_circuit_open_total {}\n\n",
            self.execution.circuit_open_events
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_half_open_total Total circuit breaker half-open transitions\n\
             # TYPE vtcode_circuit_half_open_total counter\n\
             vtcode_circuit_half_open_total {}\n\n",
            self.execution.half_open_events
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_breaker_denials_total Total circuit breaker denials\n\
             # TYPE vtcode_circuit_breaker_denials_total counter\n\
             vtcode_circuit_breaker_denials_total {}\n\n",
            self.execution.breaker_denials
        );
    }

    fn append_filtering(&self, output: &mut String) {
        // Filtering metrics
        let _ = write!(
            output,
            "# HELP vtcode_filtering_operations_total Total filtering operations\n\
             # TYPE vtcode_filtering_operations_total counter\n\
             vtcode_filtering_operations_total {}\n\n",
            self.filtering.total_operations
        );

        let _ = write!(
            output,
            "# HELP vtcode_plan_validation_rejected_total Total plan validation rejections by reason\n\
             # TYPE vtcode_plan_validation_rejected_total counter\n\
             vtcode_plan_validation_rejected_total{{reason=\"placeholder_token\"}} {}\n\n",
            self.planning.placeholder_token_rejections
        );

        let _ = write!(
            output,
            "# HELP vtcode_context_tokens_saved Estimated tokens saved by filtering\n\
             # TYPE vtcode_context_tokens_saved counter\n\
             vtcode_context_tokens_saved {}\n\n",
            self.filtering.estimated_tokens_saved()
        );
    }

    fn append_skills(&self, output: &mut String) {
        // Skills metrics
        let _ = write!(
            output,
            "# HELP vtcode_skills_total Total saved skills\n\
             # TYPE vtcode_skills_total gauge\n\
             vtcode_skills_total {}\n\n",
            self.skills.total_skills
        );

        let _ = write!(
            output,
            "# HELP vtcode_skill_reuse_ratio Ratio of skill reuse\n\
             # TYPE vtcode_skill_reuse_ratio gauge\n\
             vtcode_skill_reuse_ratio {}\n\n",
            self.skills.reuse_ratio()
        );
    }

    fn append_pii_detections(&self, output: &mut String) {
        // Security metrics
        let _ = write!(
            output,
            "# HELP vtcode_pii_detections_total Total PII patterns detected\n\
             # TYPE vtcode_pii_detections_total counter\n\
             vtcode_pii_detections_total {}\n\n",
            self.security.pii_detections
        );
    }

    fn append_provider_requests(&self, output: &mut String) {
        // Aggregate provider metrics deliberately omit provider-name labels.
        let _ = write!(
            output,
            "# HELP vtcode_provider_attempts_started_total Provider request attempts started\n\
             # TYPE vtcode_provider_attempts_started_total counter\n\
             vtcode_provider_attempts_started_total {}\n\n\
             # HELP vtcode_provider_attempts_succeeded_total Successful provider request attempts\n\
             # TYPE vtcode_provider_attempts_succeeded_total counter\n\
             vtcode_provider_attempts_succeeded_total {}\n\n\
             # HELP vtcode_provider_attempts_failed_total Failed provider request attempts\n\
             # TYPE vtcode_provider_attempts_failed_total counter\n\
             vtcode_provider_attempts_failed_total {}\n\n\
             # HELP vtcode_provider_attempts_cancelled_total Cancelled provider request attempts\n\
             # TYPE vtcode_provider_attempts_cancelled_total counter\n\
             vtcode_provider_attempts_cancelled_total {}\n\n\
             # HELP vtcode_provider_retries_scheduled_total Retryable provider failures followed by a retry\n\
             # TYPE vtcode_provider_retries_scheduled_total counter\n\
             vtcode_provider_retries_scheduled_total {}\n\n\
             # HELP vtcode_provider_attempts_in_flight Provider requests in progress, including router admission wait\n\
             # TYPE vtcode_provider_attempts_in_flight gauge\n\
             vtcode_provider_attempts_in_flight {}\n\n\
             # HELP vtcode_provider_attempt_duration_ms_total Cumulative provider request attempt duration in milliseconds\n\
             # TYPE vtcode_provider_attempt_duration_ms_total counter\n\
             vtcode_provider_attempt_duration_ms_total {}\n\n\
             # HELP vtcode_provider_time_to_first_output_ms_total Cumulative observed provider time to first output in milliseconds\n\
             # TYPE vtcode_provider_time_to_first_output_ms_total counter\n\
             vtcode_provider_time_to_first_output_ms_total {}\n\n\
             # HELP vtcode_provider_time_to_first_output_observations_total Provider attempts with a first-output observation\n\
             # TYPE vtcode_provider_time_to_first_output_observations_total counter\n\
             vtcode_provider_time_to_first_output_observations_total {}\n\n",
            self.provider_requests.attempts_started,
            self.provider_requests.attempts_succeeded,
            self.provider_requests.attempts_failed,
            self.provider_requests.attempts_cancelled,
            self.provider_requests.retries_scheduled,
            self.provider_requests.attempts_in_flight,
            self.provider_requests.total_duration_ms,
            self.provider_requests.total_time_to_first_output_ms,
            self.provider_requests.first_output_samples,
        );

        append_custom_provider_admission_prometheus(output, &self.provider_requests);
    }

    fn append_tokens_created(&self, output: &mut String) {
        let _ = write!(
            output,
            "# HELP vtcode_tokens_created_total Total PII tokens created\n\
             # TYPE vtcode_tokens_created_total counter\n\
             vtcode_tokens_created_total {}\n\n",
            self.security.tokens_created
        );
    }

    fn append_service_startup(&self, output: &mut String) {
        append_mcp_startup_prometheus(output, &self.service_startup);
    }

    fn append_acp_workspace_runtime_cache(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "# HELP vtcode_acp_workspace_runtime_cache_entries Live and in-flight ACP workspace runtime cache entries\n\
             # TYPE vtcode_acp_workspace_runtime_cache_entries gauge\n\
             vtcode_acp_workspace_runtime_cache_entries {}\n",
            self.workspace_runtime_cache_entries
        );
    }

    fn append_acp_task_lifecycle(&self, output: &mut String) {
        append_acp_task_lifecycle_prometheus(output, &self.acp_task_lifecycle);
    }
}

fn append_acp_task_lifecycle_prometheus(output: &mut String, metrics: &TaskLifecycleQueueMetrics) {
    let _ = write!(
        output,
        "# HELP vtcode_acp_task_lifecycle_queue_depth Buffered ACP task-lifecycle updates across active sessions\n\
         # TYPE vtcode_acp_task_lifecycle_queue_depth gauge\n\
         vtcode_acp_task_lifecycle_queue_depth {}\n\n\
         # HELP vtcode_acp_task_lifecycle_queue_dropped_full_total ACP task-lifecycle updates rejected by full queues\n\
         # TYPE vtcode_acp_task_lifecycle_queue_dropped_full_total counter\n\
         vtcode_acp_task_lifecycle_queue_dropped_full_total {}\n\n\
         # HELP vtcode_acp_task_lifecycle_queue_dropped_closed_total ACP task-lifecycle updates rejected by closed queues\n\
         # TYPE vtcode_acp_task_lifecycle_queue_dropped_closed_total counter\n\
         vtcode_acp_task_lifecycle_queue_dropped_closed_total {}\n\n",
        metrics.queue_depth, metrics.queue_dropped_full, metrics.queue_dropped_closed,
    );
}

fn append_mcp_startup_prometheus(output: &mut String, metrics: &ServiceStartupMetrics) {
    let _ = write!(
        output,
        "# HELP vtcode_mcp_client_startup_attempts_started_total MCP client startup attempts started\n\
         # TYPE vtcode_mcp_client_startup_attempts_started_total counter\n\
         vtcode_mcp_client_startup_attempts_started_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_succeeded_total MCP client startup attempts with all providers initialized\n\
         # TYPE vtcode_mcp_client_startup_attempts_succeeded_total counter\n\
         vtcode_mcp_client_startup_attempts_succeeded_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_partial_total MCP client startup attempts with both successful and failed providers\n\
         # TYPE vtcode_mcp_client_startup_attempts_partial_total counter\n\
         vtcode_mcp_client_startup_attempts_partial_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_failed_total MCP client startup attempts that failed\n\
         # TYPE vtcode_mcp_client_startup_attempts_failed_total counter\n\
         vtcode_mcp_client_startup_attempts_failed_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_disabled_total MCP client startup attempts skipped by configuration\n\
         # TYPE vtcode_mcp_client_startup_attempts_disabled_total counter\n\
         vtcode_mcp_client_startup_attempts_disabled_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_cancelled_total MCP client startup attempts cancelled before completion\n\
         # TYPE vtcode_mcp_client_startup_attempts_cancelled_total counter\n\
         vtcode_mcp_client_startup_attempts_cancelled_total {}\n\n\
         # HELP vtcode_mcp_client_startup_attempts_in_flight MCP client startup attempts currently running\n\
         # TYPE vtcode_mcp_client_startup_attempts_in_flight gauge\n\
         vtcode_mcp_client_startup_attempts_in_flight {}\n\n\
         # HELP vtcode_mcp_provider_startup_attempts_started_total MCP provider startup attempts started\n\
         # TYPE vtcode_mcp_provider_startup_attempts_started_total counter\n\
         vtcode_mcp_provider_startup_attempts_started_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_attempts_succeeded_total MCP provider startup attempts that succeeded\n\
         # TYPE vtcode_mcp_provider_startup_attempts_succeeded_total counter\n\
         vtcode_mcp_provider_startup_attempts_succeeded_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_attempts_failed_total MCP provider startup attempts that failed\n\
         # TYPE vtcode_mcp_provider_startup_attempts_failed_total counter\n\
         vtcode_mcp_provider_startup_attempts_failed_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_attempts_cancelled_total MCP provider startup attempts cancelled before completion\n\
         # TYPE vtcode_mcp_provider_startup_attempts_cancelled_total counter\n\
         vtcode_mcp_provider_startup_attempts_cancelled_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_attempts_in_flight MCP provider startup attempts currently running\n\
         # TYPE vtcode_mcp_provider_startup_attempts_in_flight gauge\n\
         vtcode_mcp_provider_startup_attempts_in_flight {}\n\n\
         # HELP vtcode_mcp_provider_startup_connection_failures_total MCP provider connection failures\n\
         # TYPE vtcode_mcp_provider_startup_connection_failures_total counter\n\
         vtcode_mcp_provider_startup_connection_failures_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_connection_timeouts_total MCP provider connection timeouts\n\
         # TYPE vtcode_mcp_provider_startup_connection_timeouts_total counter\n\
         vtcode_mcp_provider_startup_connection_timeouts_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_initialization_failures_total MCP provider initialization failures\n\
         # TYPE vtcode_mcp_provider_startup_initialization_failures_total counter\n\
         vtcode_mcp_provider_startup_initialization_failures_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_initialization_timeouts_total MCP provider initialization timeouts\n\
         # TYPE vtcode_mcp_provider_startup_initialization_timeouts_total counter\n\
         vtcode_mcp_provider_startup_initialization_timeouts_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_other_failures_total MCP provider startup failures in other bounded categories\n\
         # TYPE vtcode_mcp_provider_startup_other_failures_total counter\n\
         vtcode_mcp_provider_startup_other_failures_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_timeouts_total MCP provider startup timeouts\n\
         # TYPE vtcode_mcp_provider_startup_timeouts_total counter\n\
         vtcode_mcp_provider_startup_timeouts_total {}\n\n\
         # HELP vtcode_mcp_active_providers Currently active MCP providers across observed clients\n\
         # TYPE vtcode_mcp_active_providers gauge\n\
         vtcode_mcp_active_providers {}\n\n\
         # HELP vtcode_mcp_client_startup_duration_ms_total Cumulative MCP client startup duration in milliseconds\n\
         # TYPE vtcode_mcp_client_startup_duration_ms_total counter\n\
         vtcode_mcp_client_startup_duration_ms_total {}\n\n\
         # HELP vtcode_mcp_provider_startup_duration_ms_total Cumulative MCP provider startup duration in milliseconds\n\
         # TYPE vtcode_mcp_provider_startup_duration_ms_total counter\n\
         vtcode_mcp_provider_startup_duration_ms_total {}\n\n",
        metrics.client_attempts_started,
        metrics.client_attempts_succeeded,
        metrics.client_attempts_partial,
        metrics.client_attempts_failed,
        metrics.client_attempts_disabled,
        metrics.client_attempts_cancelled,
        metrics.client_attempts_in_flight,
        metrics.provider_attempts_started,
        metrics.provider_attempts_succeeded,
        metrics.provider_attempts_failed,
        metrics.provider_attempts_cancelled,
        metrics.provider_attempts_in_flight,
        metrics.provider_connection_failures,
        metrics.provider_connection_timeouts,
        metrics.provider_initialization_failures,
        metrics.provider_initialization_timeouts,
        metrics.provider_other_failures,
        metrics.provider_timeouts,
        metrics.active_providers,
        metrics.total_client_duration_ms,
        metrics.total_provider_duration_ms,
    );
}

impl ServiceStartupMetricsObserver for MetricsCollector {
    fn client_startup_started(&self) {
        let mut metrics = self.service_startup.lock();
        metrics.client_attempts_started = metrics.client_attempts_started.saturating_add(1);
        metrics.client_attempts_in_flight = metrics.client_attempts_in_flight.saturating_add(1);
    }

    fn client_startup_finished(
        &self,
        outcome: ServiceClientStartupOutcome,
        previous_active_provider_count: u64,
        active_provider_count: u64,
        duration_ms: u64,
    ) {
        let mut metrics = self.service_startup.lock();
        metrics.client_attempts_in_flight = metrics.client_attempts_in_flight.saturating_sub(1);
        match outcome {
            ServiceClientStartupOutcome::Succeeded => {
                metrics.client_attempts_succeeded = metrics.client_attempts_succeeded.saturating_add(1);
            }
            ServiceClientStartupOutcome::Partial => {
                metrics.client_attempts_partial = metrics.client_attempts_partial.saturating_add(1);
            }
            ServiceClientStartupOutcome::Failed => {
                metrics.client_attempts_failed = metrics.client_attempts_failed.saturating_add(1);
            }
            ServiceClientStartupOutcome::Disabled => {
                metrics.client_attempts_disabled = metrics.client_attempts_disabled.saturating_add(1);
            }
            ServiceClientStartupOutcome::Cancelled => {
                metrics.client_attempts_cancelled = metrics.client_attempts_cancelled.saturating_add(1);
            }
        }
        if active_provider_count >= previous_active_provider_count {
            metrics.active_providers = metrics
                .active_providers
                .saturating_add(active_provider_count.saturating_sub(previous_active_provider_count));
        } else {
            metrics.active_providers = metrics
                .active_providers
                .saturating_sub(previous_active_provider_count.saturating_sub(active_provider_count));
        }
        metrics.total_client_duration_ms = metrics.total_client_duration_ms.saturating_add(duration_ms);
    }

    fn provider_startup_started(&self) {
        let mut metrics = self.service_startup.lock();
        metrics.provider_attempts_started = metrics.provider_attempts_started.saturating_add(1);
        metrics.provider_attempts_in_flight = metrics.provider_attempts_in_flight.saturating_add(1);
    }

    fn provider_startup_finished(&self, outcome: ServiceProviderStartupOutcome, duration_ms: u64) {
        let mut metrics = self.service_startup.lock();
        metrics.provider_attempts_in_flight = metrics.provider_attempts_in_flight.saturating_sub(1);
        match outcome {
            ServiceProviderStartupOutcome::Succeeded => {
                metrics.provider_attempts_succeeded = metrics.provider_attempts_succeeded.saturating_add(1);
            }
            ServiceProviderStartupOutcome::ConnectionFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_connection_failures = metrics.provider_connection_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::ConnectionTimedOut => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_connection_timeouts = metrics.provider_connection_timeouts.saturating_add(1);
                metrics.provider_timeouts = metrics.provider_timeouts.saturating_add(1);
            }
            ServiceProviderStartupOutcome::InitializationFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_initialization_failures = metrics.provider_initialization_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::InitializationTimedOut => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_initialization_timeouts = metrics.provider_initialization_timeouts.saturating_add(1);
                metrics.provider_timeouts = metrics.provider_timeouts.saturating_add(1);
            }
            ServiceProviderStartupOutcome::OtherFailed => {
                metrics.provider_attempts_failed = metrics.provider_attempts_failed.saturating_add(1);
                metrics.provider_other_failures = metrics.provider_other_failures.saturating_add(1);
            }
            ServiceProviderStartupOutcome::Cancelled => {
                metrics.provider_attempts_cancelled = metrics.provider_attempts_cancelled.saturating_add(1);
            }
        }
        metrics.total_provider_duration_ms = metrics.total_provider_duration_ms.saturating_add(duration_ms);
    }
}

impl TaskLifecycleQueueMetricsObserver for MetricsCollector {
    fn queue_item_enqueued(&self) {
        let mut metrics = self.task_lifecycle_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_add(1);
    }

    fn queue_item_dequeued(&self) {
        let mut metrics = self.task_lifecycle_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_sub(1);
    }

    fn queue_items_drained(&self, entries: usize) {
        let entries = u64::try_from(entries).unwrap_or(u64::MAX);
        let mut metrics = self.task_lifecycle_queue.lock();
        metrics.queue_depth = metrics.queue_depth.saturating_sub(entries);
    }

    fn queue_item_dropped(&self, reason: TaskLifecycleQueueDrop) -> u64 {
        let mut metrics = self.task_lifecycle_queue.lock();
        match reason {
            TaskLifecycleQueueDrop::Full => {
                metrics.queue_dropped_full = metrics.queue_dropped_full.saturating_add(1);
                metrics.queue_dropped_full
            }
            TaskLifecycleQueueDrop::Closed => {
                metrics.queue_dropped_closed = metrics.queue_dropped_closed.saturating_add(1);
                metrics.queue_dropped_closed
            }
        }
    }
}

fn append_custom_provider_admission_prometheus(output: &mut String, provider_requests: &ProviderRequestMetrics) {
    let _ = write!(
        output,
        "# HELP vtcode_custom_provider_admission_queue_depth Requests waiting at shared custom-provider admission gates\n\
         # TYPE vtcode_custom_provider_admission_queue_depth gauge\n\
         vtcode_custom_provider_admission_queue_depth {}\n\n\
         # HELP vtcode_custom_provider_admission_in_flight Requests holding shared custom-provider admission permits\n\
         # TYPE vtcode_custom_provider_admission_in_flight gauge\n\
         vtcode_custom_provider_admission_in_flight {}\n\n\
         # HELP vtcode_custom_provider_admission_queue_waits_total Custom-provider admission queue wait observations\n\
         # TYPE vtcode_custom_provider_admission_queue_waits_total counter\n\
         vtcode_custom_provider_admission_queue_waits_total {}\n\n\
         # HELP vtcode_custom_provider_admission_queue_wait_ms_total Cumulative custom-provider admission queue wait time in milliseconds\n\
         # TYPE vtcode_custom_provider_admission_queue_wait_ms_total counter\n\
         vtcode_custom_provider_admission_queue_wait_ms_total {}\n\n\
         # HELP vtcode_custom_provider_admission_queue_timeouts_total Custom-provider requests that timed out while queued\n\
         # TYPE vtcode_custom_provider_admission_queue_timeouts_total counter\n\
         vtcode_custom_provider_admission_queue_timeouts_total {}\n\n",
        provider_requests.custom_provider_queue_depth,
        provider_requests.custom_provider_in_flight,
        provider_requests.custom_provider_queue_waits,
        provider_requests.custom_provider_queue_wait_ms,
        provider_requests.custom_provider_queue_timeouts,
    );
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collector_with_provider_attempts() -> MetricsCollector {
        let collector = MetricsCollector::new();
        collector.record_provider_attempt_started();
        collector.record_provider_attempt_failed(12, false, Some(3));
        collector.record_provider_retry_scheduled();
        collector.record_provider_attempt_finished();
        collector.record_provider_attempt_started();
        collector.record_provider_attempt_succeeded(34, Some(7));
        collector.record_provider_attempt_finished();
        collector
    }

    fn filtering_metrics_with_recorded_operation() -> FilteringMetrics {
        let collector = MetricsCollector::new();
        collector.record_filtering_operation("redact".to_string(), 80, 32, 6);
        collector.get_filtering_metrics()
    }

    #[test]
    fn test_metrics_collector_creation() {
        let collector = MetricsCollector::new();
        let summary = collector.get_summary();
        assert_eq!(summary.discovery.total_queries, 0);
        assert_eq!(summary.execution.total_executions, 0);
    }

    #[test]
    fn test_discovery_metrics_recording() {
        let collector = MetricsCollector::new();
        collector.record_discovery_query("file".to_owned(), 5, 50);

        let metrics = collector.get_discovery_metrics();
        assert_eq!(metrics.total_queries, 1);
        assert!(metrics.avg_response_time_ms() > 0);
    }

    #[test]
    fn test_execution_metrics_recording() {
        let collector = MetricsCollector::new();
        collector.record_execution_start("python3".to_owned());
        collector.record_execution_complete("python3".to_owned(), 1000, 50);

        let metrics = collector.get_execution_metrics();
        assert_eq!(metrics.total_executions, 1);
        assert_eq!(metrics.successful_executions, 1);
        assert_eq!(metrics.avg_duration_ms(), 1000);
    }

    #[test]
    fn test_metrics_summary_export() {
        let collector = MetricsCollector::new();
        collector.record_discovery_query("test".to_owned(), 3, 30);
        collector.record_pii_detection("email".to_owned());

        let summary = collector.get_summary();
        assert_eq!(summary.discovery.total_queries, 1);
        assert_eq!(summary.security.pii_detections, 1);
    }

    #[test]
    fn test_reliability_retry_metrics_recording() {
        let collector = MetricsCollector::new();
        collector.record_retry_attempt();
        collector.record_retry_success();

        let metrics = collector.get_execution_metrics();
        assert_eq!(metrics.retry_attempts, 1);
        assert_eq!(metrics.retry_successes, 1);
    }

    #[test]
    fn test_reliability_circuit_metrics_recording() {
        let collector = MetricsCollector::new();
        collector.record_circuit_open();
        collector.record_half_open();
        collector.record_breaker_denial();

        let metrics = collector.get_execution_metrics();
        assert_eq!(metrics.circuit_open_events, 1);
        assert_eq!(metrics.half_open_events, 1);
        assert_eq!(metrics.breaker_denials, 1);
    }

    #[test]
    fn test_prometheus_export() {
        let collector = MetricsCollector::new();
        collector.record_execution_complete("python3".to_owned(), 500, 40);

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_execution_total"));
        assert!(prometheus.contains("vtcode_execution_duration_ms"));
        assert!(prometheus.contains("vtcode_retry_attempts_total"));
    }

    #[test]
    fn provider_request_metrics_record_bounded_aggregate_counts_and_timings() {
        let collector = collector_with_provider_attempts();
        let metrics = collector.get_provider_request_metrics();
        let expected = ProviderRequestMetrics {
            attempts_started: 2,
            attempts_succeeded: 1,
            attempts_failed: 1,
            retries_scheduled: 1,
            total_duration_ms: 46,
            first_output_samples: 2,
            total_time_to_first_output_ms: 10,
            ..ProviderRequestMetrics::default()
        };

        assert_eq!(metrics, expected);
    }

    #[test]
    fn provider_request_prometheus_exports_aggregate_attempt_samples() {
        let collector = collector_with_provider_attempts();
        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_provider_attempts_started_total 2"));
        assert!(prometheus.contains("vtcode_provider_attempts_in_flight 0"));
        assert!(prometheus.contains("vtcode_provider_time_to_first_output_ms_total 10"));
    }

    #[test]
    fn provider_request_prometheus_exports_admission_samples() {
        let collector = collector_with_provider_attempts();
        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_depth"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_waits_total"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_timeouts_total"));
    }

    #[test]
    fn provider_request_prometheus_omits_provider_name_labels() {
        let collector = collector_with_provider_attempts();
        let prometheus = collector.export_prometheus();
        assert!(!prometheus.contains("provider=\""));
    }

    #[test]
    fn mcp_startup_metrics_export_fixed_outcomes_and_cumulative_durations() {
        let collector = MetricsCollector::new();
        collector.client_startup_started();
        for outcome in [
            ServiceProviderStartupOutcome::Succeeded,
            ServiceProviderStartupOutcome::ConnectionFailed,
            ServiceProviderStartupOutcome::ConnectionTimedOut,
            ServiceProviderStartupOutcome::InitializationFailed,
            ServiceProviderStartupOutcome::InitializationTimedOut,
            ServiceProviderStartupOutcome::OtherFailed,
            ServiceProviderStartupOutcome::Cancelled,
        ] {
            collector.provider_startup_started();
            collector.provider_startup_finished(outcome, 5);
        }
        collector.client_startup_finished(ServiceClientStartupOutcome::Partial, 0, 2, 47);

        let expected = ServiceStartupMetrics {
            client_attempts_started: 1,
            client_attempts_partial: 1,
            provider_attempts_started: 7,
            provider_attempts_succeeded: 1,
            provider_attempts_failed: 5,
            provider_attempts_cancelled: 1,
            provider_connection_failures: 1,
            provider_connection_timeouts: 1,
            provider_initialization_failures: 1,
            provider_initialization_timeouts: 1,
            provider_other_failures: 1,
            provider_timeouts: 2,
            active_providers: 2,
            total_client_duration_ms: 47,
            total_provider_duration_ms: 35,
            ..ServiceStartupMetrics::default()
        };
        assert_eq!(collector.get_service_startup_metrics(), expected);

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["mcp_startup"]["provider_timeouts"], 2);
        assert_eq!(json["mcp_startup"]["total_client_duration_ms"], 47);

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_mcp_provider_startup_attempts_started_total 7"));
        assert!(prometheus.contains("vtcode_mcp_provider_startup_timeouts_total 2"));
        assert!(prometheus.contains("vtcode_mcp_active_providers 2"));
        assert!(prometheus.contains("vtcode_mcp_client_startup_duration_ms_total 47"));
        assert!(prometheus.contains("vtcode_mcp_provider_startup_duration_ms_total 35"));
        assert!(!prometheus.contains("provider=\""));
    }

    #[test]
    fn mcp_active_provider_gauge_accumulates_client_deltas() {
        let collector = MetricsCollector::new();
        collector.client_startup_started();
        collector.client_startup_finished(ServiceClientStartupOutcome::Succeeded, 0, 2, 1);
        collector.client_startup_started();
        collector.client_startup_finished(ServiceClientStartupOutcome::Partial, 0, 1, 1);
        collector.client_startup_started();
        collector.client_startup_finished(ServiceClientStartupOutcome::Failed, 2, 0, 1);

        assert_eq!(collector.get_service_startup_metrics().active_providers, 1);
    }

    #[test]
    fn acp_workspace_runtime_cache_size_is_exported_as_a_gauge() {
        let collector = MetricsCollector::new();
        collector.set_runtime_cache_entries(RuntimeCacheKind::Workspace, 3);

        assert_eq!(collector.get_runtime_cache_entries(RuntimeCacheKind::Workspace), 3);
        assert_eq!(collector.get_summary().acp_workspace_runtime_cache_entries, 3);
        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_acp_workspace_runtime_cache_entries 3"));
    }

    #[test]
    fn acp_task_lifecycle_queue_depth_and_drops_are_exported_without_labels() {
        let collector = MetricsCollector::new();
        collector.queue_item_enqueued();
        collector.queue_item_enqueued();
        collector.queue_item_dequeued();
        collector.queue_item_dropped(TaskLifecycleQueueDrop::Full);
        collector.queue_item_dropped(TaskLifecycleQueueDrop::Closed);

        assert_eq!(
            collector.get_task_lifecycle_queue_metrics(),
            TaskLifecycleQueueMetrics {
                queue_depth: 1,
                queue_dropped_full: 1,
                queue_dropped_closed: 1,
            }
        );

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["acp_task_lifecycle"]["queue_depth"], 1);
        assert_eq!(json["acp_task_lifecycle"]["queue_dropped_full"], 1);
        assert_eq!(json["acp_task_lifecycle"]["queue_dropped_closed"], 1);

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_acp_task_lifecycle_queue_depth 1"));
        assert!(prometheus.contains("vtcode_acp_task_lifecycle_queue_dropped_full_total 1"));
        assert!(prometheus.contains("vtcode_acp_task_lifecycle_queue_dropped_closed_total 1"));
        assert!(!prometheus.contains("session_id="));
        assert!(!prometheus.contains("task_id="));
    }

    #[test]
    fn acp_task_lifecycle_shutdown_drain_clamps_queue_depth_to_zero() {
        let collector = MetricsCollector::new();
        collector.queue_item_enqueued();
        collector.queue_items_drained(2);

        assert_eq!(collector.get_task_lifecycle_queue_metrics().queue_depth, 0);
    }

    #[test]
    fn custom_provider_admission_prometheus_uses_supplied_snapshot_values() {
        let metrics = ProviderRequestMetrics {
            custom_provider_queue_depth: 2,
            custom_provider_in_flight: 3,
            custom_provider_queue_waits: 5,
            custom_provider_queue_wait_ms: 7,
            custom_provider_queue_timeouts: 11,
            ..ProviderRequestMetrics::default()
        };
        let mut prometheus = String::new();

        append_custom_provider_admission_prometheus(&mut prometheus, &metrics);

        let samples: Vec<_> = prometheus
            .lines()
            .filter(|line| !line.starts_with('#') && !line.is_empty())
            .collect();
        assert_eq!(
            samples,
            vec![
                "vtcode_custom_provider_admission_queue_depth 2",
                "vtcode_custom_provider_admission_in_flight 3",
                "vtcode_custom_provider_admission_queue_waits_total 5",
                "vtcode_custom_provider_admission_queue_wait_ms_total 7",
                "vtcode_custom_provider_admission_queue_timeouts_total 11",
            ]
        );
    }

    #[test]
    fn filtering_operation_records_operation_and_sizes() {
        let metrics = filtering_metrics_with_recorded_operation();
        let record = metrics.recent_operations.back().unwrap();
        assert_eq!(record.operation, "redact");
        assert_eq!(record.input_size_bytes, 80);
        assert_eq!(record.output_size_bytes, 32);
    }

    #[test]
    fn filtering_operation_records_duration() {
        let metrics = filtering_metrics_with_recorded_operation();
        let record = metrics.recent_operations.back().unwrap();
        assert_eq!(record.duration_ms, 6);
    }

    #[test]
    fn test_json_export() {
        let collector = MetricsCollector::new();
        collector.record_discovery_query("test".to_owned(), 2, 25);

        let json = collector.export_json().unwrap();
        assert!(json.get("timestamp").is_some());
        assert!(json.get("discovery").is_some());
    }
}
