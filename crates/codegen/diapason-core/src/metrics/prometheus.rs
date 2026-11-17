//! Renders one collected metrics snapshot as a Prometheus text payload.
//!
//! `PrometheusMetricsSnapshot` gathers every family once and each `append_*`
//! method writes one family; shared formatting helpers live in
//! `prometheus_helpers`.

use std::fmt::Write;

use super::prometheus_helpers::{
    append_custom_provider_admission_prometheus, append_mcp_startup_prometheus,
    append_task_lifecycle_notification_queue_prometheus,
};
use super::{MetricsCollector, PrometheusMetricsSnapshot, RuntimeCacheKind, ToolRecoveryEvent};

impl PrometheusMetricsSnapshot {
    /// Gather one snapshot of every metric family for Prometheus rendering.
    fn collect(collector: &MetricsCollector) -> Self {
        Self {
            discovery: collector.get_discovery_metrics(),
            execution: collector.get_execution_metrics(),
            filtering: collector.get_filtering_metrics(),
            patch_safety: collector.get_patch_safety_metrics(),
            tool_recovery: collector.get_tool_recovery_metrics(),
            planning: collector.get_planning_metrics(),
            skills: collector.get_skill_metrics(),
            security: collector.get_security_metrics(),
            provider_requests: collector.get_provider_request_metrics(),
            service_startup: collector.get_service_startup_metrics(),
            task_lifecycle_notification_queue: collector.get_task_lifecycle_notification_queue_metrics(),
            acp_extension: collector.get_acp_extension_metrics(),
            workspace_runtime_cache_entries: collector.get_runtime_cache_entries(RuntimeCacheKind::Workspace),
        }
    }

    /// Render all metric families into one Prometheus text payload.
    fn render(&self) -> String {
        let mut output = String::new();
        self.append_discovery(&mut output);
        self.append_execution(&mut output);
        self.append_filtering(&mut output);
        self.append_patch_safety(&mut output);
        self.append_tool_recovery(&mut output);
        self.append_skills(&mut output);
        self.append_pii_detections(&mut output);
        self.append_provider_requests(&mut output);
        self.append_service_startup(&mut output);
        self.append_task_lifecycle_notification_queue(&mut output);
        self.append_acp_extension(&mut output);
        self.append_workspace_runtime_cache(&mut output);
        self.append_tokens_created(&mut output);
        output
    }

    fn append_discovery(&self, output: &mut String) {
        // Discovery metrics
        let _ = write!(
            output,
            "# HELP diapason_discovery_queries_total Total tool discovery queries\n\
             # TYPE diapason_discovery_queries_total counter\n\
             diapason_discovery_queries_total {}\n\n",
            self.discovery.total_queries
        );

        let _ = write!(
            output,
            "# HELP diapason_discovery_hit_rate Hit rate of discovery queries\n\
             # TYPE diapason_discovery_hit_rate gauge\n\
             diapason_discovery_hit_rate {}\n\n",
            self.discovery.hit_rate()
        );
    }

    fn append_execution(&self, output: &mut String) {
        // Execution metrics
        let _ = write!(
            output,
            "# HELP diapason_execution_total Total code executions\n\
             # TYPE diapason_execution_total counter\n\
             diapason_execution_total {}\n\n",
            self.execution.total_executions
        );

        let _ = write!(
            output,
            "# HELP diapason_execution_duration_ms Code execution average duration\n\
             # TYPE diapason_execution_duration_ms gauge\n\
             diapason_execution_duration_ms {}\n\n",
            self.execution.avg_duration_ms()
        );

        let _ = write!(
            output,
            "# HELP diapason_retry_attempts_total Total retry attempts\n\
             # TYPE diapason_retry_attempts_total counter\n\
             diapason_retry_attempts_total {}\n\n",
            self.execution.retry_attempts
        );

        let _ = write!(
            output,
            "# HELP diapason_retry_successes_total Total retries that later succeeded\n\
             # TYPE diapason_retry_successes_total counter\n\
             diapason_retry_successes_total {}\n\n",
            self.execution.retry_successes
        );

        let _ = write!(
            output,
            "# HELP diapason_retry_exhausted_total Total operations that exhausted retries\n\
             # TYPE diapason_retry_exhausted_total counter\n\
             diapason_retry_exhausted_total {}\n\n",
            self.execution.retry_exhausted
        );

        let _ = write!(
            output,
            "# HELP diapason_circuit_open_total Total circuit breaker open transitions\n\
             # TYPE diapason_circuit_open_total counter\n\
             diapason_circuit_open_total {}\n\n",
            self.execution.circuit_open_events
        );

        let _ = write!(
            output,
            "# HELP diapason_circuit_half_open_total Total circuit breaker half-open transitions\n\
             # TYPE diapason_circuit_half_open_total counter\n\
             diapason_circuit_half_open_total {}\n\n",
            self.execution.half_open_events
        );

        let _ = write!(
            output,
            "# HELP diapason_circuit_breaker_denials_total Total circuit breaker denials\n\
             # TYPE diapason_circuit_breaker_denials_total counter\n\
             diapason_circuit_breaker_denials_total {}\n\n",
            self.execution.breaker_denials
        );
    }

    fn append_filtering(&self, output: &mut String) {
        // Filtering metrics
        let _ = write!(
            output,
            "# HELP diapason_filtering_operations_total Total filtering operations\n\
             # TYPE diapason_filtering_operations_total counter\n\
             diapason_filtering_operations_total {}\n\n",
            self.filtering.total_operations
        );

        let _ = write!(
            output,
            "# HELP diapason_plan_validation_rejected_total Total plan validation rejections by reason\n\
             # TYPE diapason_plan_validation_rejected_total counter\n\
             diapason_plan_validation_rejected_total{{reason=\"placeholder_token\"}} {}\n\n",
            self.planning.placeholder_token_rejections
        );

        let _ = write!(
            output,
            "# HELP diapason_context_tokens_saved Estimated tokens saved by filtering\n\
             # TYPE diapason_context_tokens_saved counter\n\
             diapason_context_tokens_saved {}\n\n",
            self.filtering.estimated_tokens_saved()
        );
    }

    /// Append the apply_patch precondition, no-op, and duration series.
    fn append_patch_safety(&self, output: &mut String) {
        // Patch safety metrics. Labels are fixed enum variants, never paths or
        // patch content, so the series stay bounded and content-free.
        let _ = write!(
            output,
            "# HELP diapason_apply_patch_precondition_total Total apply_patch version precondition outcomes\n\
             # TYPE diapason_apply_patch_precondition_total counter\n\
             diapason_apply_patch_precondition_total{{outcome=\"matched\"}} {}\n\
             diapason_apply_patch_precondition_total{{outcome=\"content_hash_mismatch\"}} {}\n\
             diapason_apply_patch_precondition_total{{outcome=\"invalid_scope\"}} {}\n\
             diapason_apply_patch_precondition_total{{outcome=\"malformed_hash\"}} {}\n\
             diapason_apply_patch_precondition_total{{outcome=\"missing_file\"}} {}\n\
             diapason_apply_patch_precondition_total{{outcome=\"validation_failed\"}} {}\n\n",
            self.patch_safety.precondition_matched,
            self.patch_safety.precondition_content_hash_mismatch,
            self.patch_safety.precondition_invalid_scope,
            self.patch_safety.precondition_malformed_hash,
            self.patch_safety.precondition_missing_file,
            self.patch_safety.precondition_validation_failed
        );

        let _ = write!(
            output,
            "# HELP diapason_apply_patch_no_op_total Total apply_patch no-op ladder decisions\n\
             # TYPE diapason_apply_patch_no_op_total counter\n\
             diapason_apply_patch_no_op_total{{decision=\"execute\"}} {}\n\
             diapason_apply_patch_no_op_total{{decision=\"no_op_first\"}} {}\n\
             diapason_apply_patch_no_op_total{{decision=\"no_op_repeated\"}} {}\n\
             diapason_apply_patch_no_op_total{{decision=\"no_op_blocked\"}} {}\n\n",
            self.patch_safety.no_op_execute,
            self.patch_safety.no_op_first,
            self.patch_safety.no_op_repeated,
            self.patch_safety.no_op_blocked
        );

        let _ = write!(
            output,
            "# HELP diapason_apply_patch_runtime_revalidation_total Total apply_patch runtime rechecks\n\
             # TYPE diapason_apply_patch_runtime_revalidation_total counter\n\
             diapason_apply_patch_runtime_revalidation_total{{outcome=\"matched\"}} {}\n\
             diapason_apply_patch_runtime_revalidation_total{{outcome=\"mismatch\"}} {}\n\n",
            self.patch_safety.runtime_revalidation_matched, self.patch_safety.runtime_revalidation_mismatch
        );

        let _ = write!(
            output,
            "# HELP diapason_apply_patch_certification_total Total ACP read-version certification outcomes\n\
             # TYPE diapason_apply_patch_certification_total counter\n\
             diapason_apply_patch_certification_total{{outcome=\"window_projection_mismatch\"}} {}\n\
             diapason_apply_patch_certification_total{{outcome=\"window_read_failed\"}} {}\n\
             diapason_apply_patch_certification_total{{outcome=\"window_not_utf8\"}} {}\n\
             diapason_apply_patch_certification_total{{outcome=\"read_content_mismatch\"}} {}\n\
             diapason_apply_patch_certification_total{{outcome=\"read_version_capture_failed\"}} {}\n\n",
            self.patch_safety.certification_window_projection_mismatch,
            self.patch_safety.certification_window_read_failed,
            self.patch_safety.certification_window_not_utf8,
            self.patch_safety.certification_read_content_mismatch,
            self.patch_safety.certification_read_version_capture_failed
        );

        let _ = write!(
            output,
            "# HELP diapason_apply_patch_failure_guard_total Total apply_patch repeated-failure-guard outcomes\n\
             # TYPE diapason_apply_patch_failure_guard_total counter\n\
             diapason_apply_patch_failure_guard_total{{outcome=\"attempt_failed\"}} {}\n\
             diapason_apply_patch_failure_guard_total{{outcome=\"blocked\"}} {}\n\n",
            self.patch_safety.failure_guard_attempt_failed, self.patch_safety.failure_guard_blocked
        );

        let _ = write!(
            output,
            "# HELP diapason_apply_patch_evaluations_total Total evaluated apply_patch requests\n\
             # TYPE diapason_apply_patch_evaluations_total counter\n\
             diapason_apply_patch_evaluations_total {}\n\n\
             # HELP diapason_apply_patch_avg_duration_ms Average apply_patch evaluation duration\n\
             # TYPE diapason_apply_patch_avg_duration_ms gauge\n\
             diapason_apply_patch_avg_duration_ms {}\n\n",
            self.patch_safety.evaluations,
            self.patch_safety.avg_duration_ms()
        );
    }

    /// Append the bounded tool-recovery event counters.
    ///
    /// The event vocabulary is transport-neutral: core records the events it
    /// originates and adapters map their own observations onto the same
    /// variants, so the series carries no ACP-specific label.
    fn append_tool_recovery(&self, output: &mut String) {
        // Labels are fixed event variants, so neither tool names nor call
        // identifiers can reach the series.
        let _ = writeln!(
            output,
            "# HELP diapason_tool_recovery_total Total tool-recovery events recorded by the agent runtime\n\
             # TYPE diapason_tool_recovery_total counter"
        );
        for event in ToolRecoveryEvent::ALL {
            let _ = writeln!(
                output,
                "diapason_tool_recovery_total{{event=\"{}\"}} {}",
                event.as_str(),
                self.tool_recovery.count(event)
            );
        }
        output.push('\n');
    }

    fn append_skills(&self, output: &mut String) {
        // Skills metrics
        let _ = write!(
            output,
            "# HELP diapason_skills_total Total saved skills\n\
             # TYPE diapason_skills_total gauge\n\
             diapason_skills_total {}\n\n",
            self.skills.total_skills
        );

        let _ = write!(
            output,
            "# HELP diapason_skill_reuse_ratio Ratio of skill reuse\n\
             # TYPE diapason_skill_reuse_ratio gauge\n\
             diapason_skill_reuse_ratio {}\n\n",
            self.skills.reuse_ratio()
        );
    }

    fn append_pii_detections(&self, output: &mut String) {
        // Security metrics
        let _ = write!(
            output,
            "# HELP diapason_pii_detections_total Total PII patterns detected\n\
             # TYPE diapason_pii_detections_total counter\n\
             diapason_pii_detections_total {}\n\n",
            self.security.pii_detections
        );
    }

    fn append_provider_requests(&self, output: &mut String) {
        // Aggregate provider metrics deliberately omit provider-name labels.
        let _ = write!(
            output,
            "# HELP diapason_provider_attempts_started_total Provider request attempts started\n\
             # TYPE diapason_provider_attempts_started_total counter\n\
             diapason_provider_attempts_started_total {}\n\n\
             # HELP diapason_provider_attempts_succeeded_total Successful provider request attempts\n\
             # TYPE diapason_provider_attempts_succeeded_total counter\n\
             diapason_provider_attempts_succeeded_total {}\n\n\
             # HELP diapason_provider_attempts_failed_total Failed provider request attempts\n\
             # TYPE diapason_provider_attempts_failed_total counter\n\
             diapason_provider_attempts_failed_total {}\n\n\
             # HELP diapason_provider_attempts_cancelled_total Cancelled provider request attempts\n\
             # TYPE diapason_provider_attempts_cancelled_total counter\n\
             diapason_provider_attempts_cancelled_total {}\n\n\
             # HELP diapason_provider_retries_scheduled_total Retryable provider failures followed by a retry\n\
             # TYPE diapason_provider_retries_scheduled_total counter\n\
             diapason_provider_retries_scheduled_total {}\n\n\
             # HELP diapason_provider_attempts_in_flight Provider requests in progress, including router admission wait\n\
             # TYPE diapason_provider_attempts_in_flight gauge\n\
             diapason_provider_attempts_in_flight {}\n\n\
             # HELP diapason_provider_attempt_duration_ms_total Cumulative provider request attempt duration in milliseconds\n\
             # TYPE diapason_provider_attempt_duration_ms_total counter\n\
             diapason_provider_attempt_duration_ms_total {}\n\n\
             # HELP diapason_provider_time_to_first_output_ms_total Cumulative observed provider time to first output in milliseconds\n\
             # TYPE diapason_provider_time_to_first_output_ms_total counter\n\
             diapason_provider_time_to_first_output_ms_total {}\n\n\
             # HELP diapason_provider_time_to_first_output_observations_total Provider attempts with a first-output observation\n\
             # TYPE diapason_provider_time_to_first_output_observations_total counter\n\
             diapason_provider_time_to_first_output_observations_total {}\n\n",
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
            "# HELP diapason_tokens_created_total Total PII tokens created\n\
             # TYPE diapason_tokens_created_total counter\n\
             diapason_tokens_created_total {}\n\n",
            self.security.tokens_created
        );
    }

    fn append_service_startup(&self, output: &mut String) {
        append_mcp_startup_prometheus(output, &self.service_startup);
    }

    fn append_workspace_runtime_cache(&self, output: &mut String) {
        let _ = writeln!(
            output,
            "# HELP diapason_workspace_runtime_cache_entries Live and in-flight workspace runtime cache entries\n\
             # TYPE diapason_workspace_runtime_cache_entries gauge\n\
             diapason_workspace_runtime_cache_entries {}\n",
            self.workspace_runtime_cache_entries
        );
    }

    fn append_task_lifecycle_notification_queue(&self, output: &mut String) {
        append_task_lifecycle_notification_queue_prometheus(output, &self.task_lifecycle_notification_queue);
    }
}

impl MetricsCollector {
    /// Export metrics in Prometheus format
    pub fn export_prometheus(&self) -> String {
        PrometheusMetricsSnapshot::collect(self).render()
    }
}
