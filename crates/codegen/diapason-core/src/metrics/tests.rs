//! Unit tests for collector recording, export, and service metric families.

use super::*;

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

/// Fixes the bounded recovery series that dashboards and alerts consume.
///
/// Every event is recorded exactly once, so the snapshot pins both the
/// fixed label set and its rendering order without introducing a
/// nondeterministic value.
#[test]
fn tool_recovery_prometheus_series_is_stable() {
    let collector = MetricsCollector::new();
    for event in ToolRecoveryEvent::ALL {
        collector.record_tool_recovery(event);
    }

    let series = collector
        .export_prometheus()
        .lines()
        .skip_while(|line| !line.starts_with("# HELP diapason_tool_recovery_total"))
        .take_while(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(series, @r###"
    # HELP diapason_tool_recovery_total Total tool-recovery events recorded by the agent runtime
    # TYPE diapason_tool_recovery_total counter
    diapason_tool_recovery_total{event="staged_call"} 1
    diapason_tool_recovery_total{event="replaced_call"} 1
    diapason_tool_recovery_total{event="interrupted_call"} 1
    diapason_tool_recovery_total{event="relocated_result"} 1
    diapason_tool_recovery_total{event="repaired_archive"} 1
    diapason_tool_recovery_total{event="ambiguous_history"} 1
    diapason_tool_recovery_total{event="concurrent_prompt_rejection"} 1
    diapason_tool_recovery_total{event="stale_write_stdin_request"} 1
    diapason_tool_recovery_total{event="checkpoint_failure"} 1
    diapason_tool_recovery_total{event="checkpoint_skipped"} 1
    diapason_tool_recovery_total{event="duplicate_tool_call_batch"} 1
    "###);
}

#[test]
fn test_prometheus_export() {
    let collector = MetricsCollector::new();
    collector.record_execution_complete("python3".to_owned(), 500, 40);

    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_execution_total"));
    assert!(prometheus.contains("diapason_execution_duration_ms"));
    assert!(prometheus.contains("diapason_retry_attempts_total"));
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
    assert!(prometheus.contains("diapason_mcp_provider_startup_attempts_started_total 7"));
    assert!(prometheus.contains("diapason_mcp_provider_startup_timeouts_total 2"));
    assert!(prometheus.contains("diapason_mcp_active_providers 2"));
    assert!(prometheus.contains("diapason_mcp_client_startup_duration_ms_total 47"));
    assert!(prometheus.contains("diapason_mcp_provider_startup_duration_ms_total 35"));
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
fn workspace_runtime_cache_size_is_exported_as_a_gauge() {
    let collector = MetricsCollector::new();
    collector.set_runtime_cache_entries(RuntimeCacheKind::Workspace, 3);

    assert_eq!(collector.get_runtime_cache_entries(RuntimeCacheKind::Workspace), 3);
    assert_eq!(collector.get_summary().workspace_runtime_cache_entries, 3);
    let json = collector.export_json().expect("metrics JSON should serialize");
    assert_eq!(json["workspace_runtime_cache_entries"], 3);
    assert!(json.get("acp_workspace_runtime_cache_entries").is_none());
    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_workspace_runtime_cache_entries 3"));
    assert!(!prometheus.contains("diapason_acp_workspace_runtime_cache_entries"));
}

#[test]
fn task_lifecycle_notification_queue_metrics_are_exported_without_labels() {
    let collector = MetricsCollector::new();
    collector.queue_item_enqueued();
    collector.queue_item_enqueued();
    collector.queue_item_dequeued();
    collector.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Full);
    collector.queue_item_dropped(TaskLifecycleNotificationQueueDrop::Closed);

    assert_eq!(
        collector.get_task_lifecycle_notification_queue_metrics(),
        TaskLifecycleNotificationQueueMetrics {
            queue_depth: 1,
            queue_dropped_full: 1,
            queue_dropped_closed: 1,
        }
    );

    let json = collector.export_json().expect("metrics JSON should serialize");
    assert_eq!(json["task_lifecycle_notification_queue"]["queue_depth"], 1);
    assert_eq!(json["task_lifecycle_notification_queue"]["queue_dropped_full"], 1);
    assert_eq!(json["task_lifecycle_notification_queue"]["queue_dropped_closed"], 1);
    assert!(json.get("acp_task_lifecycle").is_none());

    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_task_lifecycle_notification_queue_depth 1"));
    assert!(prometheus.contains("diapason_task_lifecycle_notification_queue_dropped_full_total 1"));
    assert!(prometheus.contains("diapason_task_lifecycle_notification_queue_dropped_closed_total 1"));
    assert!(!prometheus.contains("diapason_acp_task_lifecycle"));
    assert!(!prometheus.contains("session_id="));
    assert!(!prometheus.contains("task_id="));
}

#[test]
fn task_lifecycle_notification_queue_shutdown_drain_clamps_depth_to_zero() {
    let collector = MetricsCollector::new();
    collector.queue_item_enqueued();
    collector.queue_items_drained(2);

    assert_eq!(collector.get_task_lifecycle_notification_queue_metrics().queue_depth, 0);
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
