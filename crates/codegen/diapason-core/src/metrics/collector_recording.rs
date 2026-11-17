//! Recording methods that update each metric family on the collector.
//!
//! Every `record_*` entry point lives here; the read-side and summary methods
//! remain in the sibling module so recording and reporting stay separate.

use std::sync::Arc;

use super::{
    MetricsCollector, PatchCertificationOutcome, PatchFailureGuardOutcome, PatchNoOpOutcome, PatchPreconditionOutcome,
    PlanValidationRejectionReason, ToolRecoveryEvent,
};

impl MetricsCollector {
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

    // ========== Patch Safety Metrics ==========

    /// Record one bounded `apply_patch` version-precondition outcome.
    pub fn record_patch_precondition(&self, outcome: PatchPreconditionOutcome) {
        self.patch_safety.lock().record_precondition(outcome);
    }

    /// Record one runtime `apply_patch` recheck, which is counted separately
    /// from the handler preflight so one logical request is never double
    /// counted in the preflight series.
    pub fn record_runtime_revalidation(&self, matched: bool) {
        self.patch_safety.lock().record_runtime_revalidation(matched);
    }

    /// Record one ACP read-version certification outcome.
    pub fn record_patch_certification(&self, outcome: PatchCertificationOutcome) {
        self.patch_safety.lock().record_certification(outcome);
    }

    /// Record one bounded repeated no-op ladder decision.
    pub fn record_patch_no_op(&self, decision: PatchNoOpOutcome) {
        self.patch_safety.lock().record_no_op(decision);
    }

    /// Record one repeated-failure-guard outcome.
    pub fn record_patch_failure_guard(&self, outcome: PatchFailureGuardOutcome) {
        self.patch_safety.lock().record_failure_guard(outcome);
    }

    /// Record one completed `apply_patch` evaluation and its duration.
    pub fn record_patch_evaluation(&self, duration_ms: u64) {
        self.patch_safety.lock().record_evaluation(duration_ms);
    }

    /// Record one bounded tool-recovery event.
    pub fn record_tool_recovery(&self, event: ToolRecoveryEvent) {
        self.tool_recovery.lock().record(event);
    }

    /// Record `count` occurrences of one bounded tool-recovery event.
    pub fn record_tool_recovery_events(&self, event: ToolRecoveryEvent, count: usize) {
        self.tool_recovery.lock().record_many(event, count);
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
}
