//! Prometheus text fragments shared by the snapshot's append methods.
//!
//! These families are formatted by free functions so the method that triggers
//! them reads as a single delegation.

use std::fmt::Write;

use super::{ProviderRequestMetrics, ServiceStartupMetrics, TaskLifecycleNotificationQueueMetrics};

pub(super) fn append_task_lifecycle_notification_queue_prometheus(
    output: &mut String,
    metrics: &TaskLifecycleNotificationQueueMetrics,
) {
    let _ = write!(
        output,
        "# HELP diapason_task_lifecycle_notification_queue_depth Buffered task-lifecycle updates across active sessions\n\
         # TYPE diapason_task_lifecycle_notification_queue_depth gauge\n\
         diapason_task_lifecycle_notification_queue_depth {}\n\n\
         # HELP diapason_task_lifecycle_notification_queue_dropped_full_total Task-lifecycle updates rejected by full queues\n\
         # TYPE diapason_task_lifecycle_notification_queue_dropped_full_total counter\n\
         diapason_task_lifecycle_notification_queue_dropped_full_total {}\n\n\
         # HELP diapason_task_lifecycle_notification_queue_dropped_closed_total Task-lifecycle updates rejected by closed queues\n\
         # TYPE diapason_task_lifecycle_notification_queue_dropped_closed_total counter\n\
         diapason_task_lifecycle_notification_queue_dropped_closed_total {}\n\n",
        metrics.queue_depth, metrics.queue_dropped_full, metrics.queue_dropped_closed,
    );
}

pub(super) fn append_mcp_startup_prometheus(output: &mut String, metrics: &ServiceStartupMetrics) {
    let _ = write!(
        output,
        "# HELP diapason_mcp_client_startup_attempts_started_total MCP client startup attempts started\n\
         # TYPE diapason_mcp_client_startup_attempts_started_total counter\n\
         diapason_mcp_client_startup_attempts_started_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_succeeded_total MCP client startup attempts with all providers initialized\n\
         # TYPE diapason_mcp_client_startup_attempts_succeeded_total counter\n\
         diapason_mcp_client_startup_attempts_succeeded_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_partial_total MCP client startup attempts with both successful and failed providers\n\
         # TYPE diapason_mcp_client_startup_attempts_partial_total counter\n\
         diapason_mcp_client_startup_attempts_partial_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_failed_total MCP client startup attempts that failed\n\
         # TYPE diapason_mcp_client_startup_attempts_failed_total counter\n\
         diapason_mcp_client_startup_attempts_failed_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_disabled_total MCP client startup attempts skipped by configuration\n\
         # TYPE diapason_mcp_client_startup_attempts_disabled_total counter\n\
         diapason_mcp_client_startup_attempts_disabled_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_cancelled_total MCP client startup attempts cancelled before completion\n\
         # TYPE diapason_mcp_client_startup_attempts_cancelled_total counter\n\
         diapason_mcp_client_startup_attempts_cancelled_total {}\n\n\
         # HELP diapason_mcp_client_startup_attempts_in_flight MCP client startup attempts currently running\n\
         # TYPE diapason_mcp_client_startup_attempts_in_flight gauge\n\
         diapason_mcp_client_startup_attempts_in_flight {}\n\n\
         # HELP diapason_mcp_provider_startup_attempts_started_total MCP provider startup attempts started\n\
         # TYPE diapason_mcp_provider_startup_attempts_started_total counter\n\
         diapason_mcp_provider_startup_attempts_started_total {}\n\n\
         # HELP diapason_mcp_provider_startup_attempts_succeeded_total MCP provider startup attempts that succeeded\n\
         # TYPE diapason_mcp_provider_startup_attempts_succeeded_total counter\n\
         diapason_mcp_provider_startup_attempts_succeeded_total {}\n\n\
         # HELP diapason_mcp_provider_startup_attempts_failed_total MCP provider startup attempts that failed\n\
         # TYPE diapason_mcp_provider_startup_attempts_failed_total counter\n\
         diapason_mcp_provider_startup_attempts_failed_total {}\n\n\
         # HELP diapason_mcp_provider_startup_attempts_cancelled_total MCP provider startup attempts cancelled before completion\n\
         # TYPE diapason_mcp_provider_startup_attempts_cancelled_total counter\n\
         diapason_mcp_provider_startup_attempts_cancelled_total {}\n\n\
         # HELP diapason_mcp_provider_startup_attempts_in_flight MCP provider startup attempts currently running\n\
         # TYPE diapason_mcp_provider_startup_attempts_in_flight gauge\n\
         diapason_mcp_provider_startup_attempts_in_flight {}\n\n\
         # HELP diapason_mcp_provider_startup_connection_failures_total MCP provider connection failures\n\
         # TYPE diapason_mcp_provider_startup_connection_failures_total counter\n\
         diapason_mcp_provider_startup_connection_failures_total {}\n\n\
         # HELP diapason_mcp_provider_startup_connection_timeouts_total MCP provider connection timeouts\n\
         # TYPE diapason_mcp_provider_startup_connection_timeouts_total counter\n\
         diapason_mcp_provider_startup_connection_timeouts_total {}\n\n\
         # HELP diapason_mcp_provider_startup_initialization_failures_total MCP provider initialization failures\n\
         # TYPE diapason_mcp_provider_startup_initialization_failures_total counter\n\
         diapason_mcp_provider_startup_initialization_failures_total {}\n\n\
         # HELP diapason_mcp_provider_startup_initialization_timeouts_total MCP provider initialization timeouts\n\
         # TYPE diapason_mcp_provider_startup_initialization_timeouts_total counter\n\
         diapason_mcp_provider_startup_initialization_timeouts_total {}\n\n\
         # HELP diapason_mcp_provider_startup_other_failures_total MCP provider startup failures in other bounded categories\n\
         # TYPE diapason_mcp_provider_startup_other_failures_total counter\n\
         diapason_mcp_provider_startup_other_failures_total {}\n\n\
         # HELP diapason_mcp_provider_startup_timeouts_total MCP provider startup timeouts\n\
         # TYPE diapason_mcp_provider_startup_timeouts_total counter\n\
         diapason_mcp_provider_startup_timeouts_total {}\n\n\
         # HELP diapason_mcp_active_providers Currently active MCP providers across observed clients\n\
         # TYPE diapason_mcp_active_providers gauge\n\
         diapason_mcp_active_providers {}\n\n\
         # HELP diapason_mcp_client_startup_duration_ms_total Cumulative MCP client startup duration in milliseconds\n\
         # TYPE diapason_mcp_client_startup_duration_ms_total counter\n\
         diapason_mcp_client_startup_duration_ms_total {}\n\n\
         # HELP diapason_mcp_provider_startup_duration_ms_total Cumulative MCP provider startup duration in milliseconds\n\
         # TYPE diapason_mcp_provider_startup_duration_ms_total counter\n\
         diapason_mcp_provider_startup_duration_ms_total {}\n\n",
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

pub(super) fn append_custom_provider_admission_prometheus(
    output: &mut String,
    provider_requests: &ProviderRequestMetrics,
) {
    let _ = write!(
        output,
        "# HELP diapason_custom_provider_admission_queue_depth Requests waiting at shared custom-provider admission gates\n\
         # TYPE diapason_custom_provider_admission_queue_depth gauge\n\
         diapason_custom_provider_admission_queue_depth {}\n\n\
         # HELP diapason_custom_provider_admission_in_flight Requests holding shared custom-provider admission permits\n\
         # TYPE diapason_custom_provider_admission_in_flight gauge\n\
         diapason_custom_provider_admission_in_flight {}\n\n\
         # HELP diapason_custom_provider_admission_queue_waits_total Custom-provider admission queue wait observations\n\
         # TYPE diapason_custom_provider_admission_queue_waits_total counter\n\
         diapason_custom_provider_admission_queue_waits_total {}\n\n\
         # HELP diapason_custom_provider_admission_queue_wait_ms_total Cumulative custom-provider admission queue wait time in milliseconds\n\
         # TYPE diapason_custom_provider_admission_queue_wait_ms_total counter\n\
         diapason_custom_provider_admission_queue_wait_ms_total {}\n\n\
         # HELP diapason_custom_provider_admission_queue_timeouts_total Custom-provider requests that timed out while queued\n\
         # TYPE diapason_custom_provider_admission_queue_timeouts_total counter\n\
         diapason_custom_provider_admission_queue_timeouts_total {}\n\n",
        provider_requests.custom_provider_queue_depth,
        provider_requests.custom_provider_in_flight,
        provider_requests.custom_provider_queue_waits,
        provider_requests.custom_provider_queue_wait_ms,
        provider_requests.custom_provider_queue_timeouts,
    );
}
