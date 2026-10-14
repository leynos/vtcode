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
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
        let discovery = self.get_discovery_metrics();
        let execution = self.get_execution_metrics();
        let filtering = self.get_filtering_metrics();
        let planning = self.get_planning_metrics();
        let skills = self.get_skill_metrics();
        let security = self.get_security_metrics();
        let provider_requests = self.get_provider_request_metrics();

        let mut output = String::new();

        // Discovery metrics
        let _ = write!(
            output,
            "# HELP vtcode_discovery_queries_total Total tool discovery queries\n\
             # TYPE vtcode_discovery_queries_total counter\n\
             vtcode_discovery_queries_total {}\n\n",
            discovery.total_queries
        );

        let _ = write!(
            output,
            "# HELP vtcode_discovery_hit_rate Hit rate of discovery queries\n\
             # TYPE vtcode_discovery_hit_rate gauge\n\
             vtcode_discovery_hit_rate {}\n\n",
            discovery.hit_rate()
        );

        // Execution metrics
        let _ = write!(
            output,
            "# HELP vtcode_execution_total Total code executions\n\
             # TYPE vtcode_execution_total counter\n\
             vtcode_execution_total {}\n\n",
            execution.total_executions
        );

        let _ = write!(
            output,
            "# HELP vtcode_execution_duration_ms Code execution average duration\n\
             # TYPE vtcode_execution_duration_ms gauge\n\
             vtcode_execution_duration_ms {}\n\n",
            execution.avg_duration_ms()
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_attempts_total Total retry attempts\n\
             # TYPE vtcode_retry_attempts_total counter\n\
             vtcode_retry_attempts_total {}\n\n",
            execution.retry_attempts
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_successes_total Total retries that later succeeded\n\
             # TYPE vtcode_retry_successes_total counter\n\
             vtcode_retry_successes_total {}\n\n",
            execution.retry_successes
        );

        let _ = write!(
            output,
            "# HELP vtcode_retry_exhausted_total Total operations that exhausted retries\n\
             # TYPE vtcode_retry_exhausted_total counter\n\
             vtcode_retry_exhausted_total {}\n\n",
            execution.retry_exhausted
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_open_total Total circuit breaker open transitions\n\
             # TYPE vtcode_circuit_open_total counter\n\
             vtcode_circuit_open_total {}\n\n",
            execution.circuit_open_events
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_half_open_total Total circuit breaker half-open transitions\n\
             # TYPE vtcode_circuit_half_open_total counter\n\
             vtcode_circuit_half_open_total {}\n\n",
            execution.half_open_events
        );

        let _ = write!(
            output,
            "# HELP vtcode_circuit_breaker_denials_total Total circuit breaker denials\n\
             # TYPE vtcode_circuit_breaker_denials_total counter\n\
             vtcode_circuit_breaker_denials_total {}\n\n",
            execution.breaker_denials
        );

        // Filtering metrics
        let _ = write!(
            output,
            "# HELP vtcode_filtering_operations_total Total filtering operations\n\
             # TYPE vtcode_filtering_operations_total counter\n\
             vtcode_filtering_operations_total {}\n\n",
            filtering.total_operations
        );

        let _ = write!(
            output,
            "# HELP vtcode_plan_validation_rejected_total Total plan validation rejections by reason\n\
             # TYPE vtcode_plan_validation_rejected_total counter\n\
             vtcode_plan_validation_rejected_total{{reason=\"placeholder_token\"}} {}\n\n",
            planning.placeholder_token_rejections
        );

        let _ = write!(
            output,
            "# HELP vtcode_context_tokens_saved Estimated tokens saved by filtering\n\
             # TYPE vtcode_context_tokens_saved counter\n\
             vtcode_context_tokens_saved {}\n\n",
            filtering.estimated_tokens_saved()
        );

        // Skills metrics
        let _ = write!(
            output,
            "# HELP vtcode_skills_total Total saved skills\n\
             # TYPE vtcode_skills_total gauge\n\
             vtcode_skills_total {}\n\n",
            skills.total_skills
        );

        let _ = write!(
            output,
            "# HELP vtcode_skill_reuse_ratio Ratio of skill reuse\n\
             # TYPE vtcode_skill_reuse_ratio gauge\n\
             vtcode_skill_reuse_ratio {}\n\n",
            skills.reuse_ratio()
        );

        // Security metrics
        let _ = write!(
            output,
            "# HELP vtcode_pii_detections_total Total PII patterns detected\n\
             # TYPE vtcode_pii_detections_total counter\n\
             vtcode_pii_detections_total {}\n\n",
            security.pii_detections
        );

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
            provider_requests.attempts_started,
            provider_requests.attempts_succeeded,
            provider_requests.attempts_failed,
            provider_requests.attempts_cancelled,
            provider_requests.retries_scheduled,
            provider_requests.attempts_in_flight,
            provider_requests.total_duration_ms,
            provider_requests.total_time_to_first_output_ms,
            provider_requests.first_output_samples,
        );

        append_custom_provider_admission_prometheus(&mut output, &provider_requests);

        let _ = write!(
            output,
            "# HELP vtcode_tokens_created_total Total PII tokens created\n\
             # TYPE vtcode_tokens_created_total counter\n\
             vtcode_tokens_created_total {}\n\n",
            security.tokens_created
        );

        output
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
    fn test_reliability_metrics_recording() {
        let collector = MetricsCollector::new();
        collector.record_retry_attempt();
        collector.record_retry_success();
        collector.record_circuit_open();
        collector.record_half_open();
        collector.record_breaker_denial();

        let metrics = collector.get_execution_metrics();
        assert_eq!(metrics.retry_attempts, 1);
        assert_eq!(metrics.retry_successes, 1);
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
    fn provider_request_metrics_export_bounded_aggregate_counts_and_timings() {
        let collector = MetricsCollector::new();
        collector.record_provider_attempt_started();
        collector.record_provider_attempt_failed(12, false, Some(3));
        collector.record_provider_retry_scheduled();
        collector.record_provider_attempt_finished();
        collector.record_provider_attempt_started();
        collector.record_provider_attempt_succeeded(34, Some(7));
        collector.record_provider_attempt_finished();

        let metrics = collector.get_provider_request_metrics();
        assert_eq!(metrics.attempts_started, 2);
        assert_eq!(metrics.attempts_failed, 1);
        assert_eq!(metrics.attempts_succeeded, 1);
        assert_eq!(metrics.retries_scheduled, 1);
        assert_eq!(metrics.attempts_in_flight, 0);
        assert_eq!(metrics.total_duration_ms, 46);
        assert_eq!(metrics.first_output_samples, 2);
        assert_eq!(metrics.total_time_to_first_output_ms, 10);

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains("vtcode_provider_attempts_started_total 2"));
        assert!(prometheus.contains("vtcode_provider_attempts_in_flight 0"));
        assert!(prometheus.contains("vtcode_provider_time_to_first_output_ms_total 10"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_depth"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_waits_total"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_timeouts_total"));
        assert!(!prometheus.contains("provider=\""));
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

        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_depth 2\n"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_in_flight 3\n"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_waits_total 5\n"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_wait_ms_total 7\n"));
        assert!(prometheus.contains("vtcode_custom_provider_admission_queue_timeouts_total 11\n"));
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
