//! Unit tests for the Lody ACP extension metrics family.

use crate::metrics::MetricsCollector;
use std::time::Duration;

/// Pins the ACP extension series that dashboards consume.
///
/// The counters are bounded aggregates with no session or task labels, so
/// the rendered values are deterministic for a fixed recording sequence.
#[test]
fn acp_extension_prometheus_series_is_stable() {
    let collector = MetricsCollector::new();
    collector.begin_acp_management().finish(true);
    collector.begin_acp_management().finish(false);
    collector.record_acp_usage_notification(true);
    collector.record_acp_usage_notification(true);
    collector.record_acp_usage_notification(false);
    collector.record_acp_initial_snapshots_enqueued(2);
    collector.record_acp_initial_snapshot_rejected();

    let series = collector
        .export_prometheus()
        .lines()
        .filter(|line| {
            // The guard measures real elapsed time, so the duration value
            // is asserted separately rather than pinned in the snapshot.
            !line.starts_with("diapason_acp_management_duration_ms_total ")
                && (line.starts_with("# HELP diapason_acp_")
                    || line.starts_with("# TYPE diapason_acp_")
                    || line.starts_with("diapason_acp_"))
        })
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(series, @r###"
    # HELP diapason_acp_management_requests_started_total Lody ACP management requests started (list, cancel and output)
    # TYPE diapason_acp_management_requests_started_total counter
    diapason_acp_management_requests_started_total 2
    # HELP diapason_acp_management_requests_succeeded_total Lody ACP management requests that succeeded
    # TYPE diapason_acp_management_requests_succeeded_total counter
    diapason_acp_management_requests_succeeded_total 1
    # HELP diapason_acp_management_requests_failed_total Lody ACP management requests that failed
    # TYPE diapason_acp_management_requests_failed_total counter
    diapason_acp_management_requests_failed_total 1
    # HELP diapason_acp_management_requests_in_flight Lody ACP management requests currently running
    # TYPE diapason_acp_management_requests_in_flight gauge
    diapason_acp_management_requests_in_flight 0
    # HELP diapason_acp_management_duration_ms_total Total time spent serving Lody ACP management requests
    # TYPE diapason_acp_management_duration_ms_total counter
    # HELP diapason_acp_usage_notifications_enqueued_total Lody ACP usage notifications enqueued for delivery
    # TYPE diapason_acp_usage_notifications_enqueued_total counter
    diapason_acp_usage_notifications_enqueued_total 2
    # HELP diapason_acp_usage_notifications_failed_total Lody ACP usage notification publication failures, including serialization before enqueue
    # TYPE diapason_acp_usage_notifications_failed_total counter
    diapason_acp_usage_notifications_failed_total 1
    # HELP diapason_acp_initial_snapshots_enqueued_total Lody ACP initial task snapshots accepted onto the lifecycle queue
    # TYPE diapason_acp_initial_snapshots_enqueued_total counter
    diapason_acp_initial_snapshots_enqueued_total 2
    # HELP diapason_acp_initial_snapshots_rejected_total Lody ACP initial task snapshots rejected by the lifecycle queue
    # TYPE diapason_acp_initial_snapshots_rejected_total counter
    diapason_acp_initial_snapshots_rejected_total 1
    "###);

    // The guard measures real elapsed time, so the duration value is not
    // part of the fixed snapshot. The rendered series must still carry the
    // aggregate the collector recorded, and parse as a number.
    let duration_line = collector
        .export_prometheus()
        .lines()
        .find(|line| line.starts_with("diapason_acp_management_duration_ms_total "))
        .expect("the ACP management duration series must be rendered")
        .to_string();
    let total_ms: u64 = duration_line
        .rsplit(' ')
        .next()
        .and_then(|value| value.parse().ok())
        .expect("the ACP management duration value must be a number");
    assert_eq!(
        total_ms,
        collector.get_acp_extension_metrics().management_total_duration_ms,
        "the exported duration series must match the recorded aggregate: {duration_line}"
    );
}

#[test]
fn acp_management_guard_records_a_completed_outcome_once() {
    let collector = MetricsCollector::new();
    let guard = collector.begin_acp_management();
    assert_eq!(
        collector.get_acp_extension_metrics().management_requests_in_flight,
        1,
        "a guard must raise the in-flight gauge when it is created"
    );

    guard.finish(true);
    let metrics = collector.get_acp_extension_metrics();
    assert_eq!(metrics.management_requests_in_flight, 0, "finishing must release the gauge exactly once");
    assert_eq!(metrics.management_requests_started, 1);
    assert_eq!(metrics.management_requests_succeeded, 1);
    assert_eq!(metrics.management_requests_failed, 0);
}

#[test]
fn acp_management_guard_records_a_dropped_request_as_a_failure() {
    let collector = MetricsCollector::new();
    drop(collector.begin_acp_management());

    let metrics = collector.get_acp_extension_metrics();
    assert_eq!(
        metrics.management_requests_in_flight, 0,
        "a dropped guard must not leave the in-flight gauge elevated"
    );
    assert_eq!(
        metrics.management_requests_failed, 1,
        "a cancelled request must be counted as a failure rather than vanish"
    );
    assert_eq!(
        metrics.management_requests_succeeded + metrics.management_requests_failed,
        metrics.management_requests_started,
        "every started request must record exactly one outcome"
    );
}

#[test]
fn acp_management_guard_debug_omits_discovery_query_keywords() {
    let collector = MetricsCollector::new();
    collector.record_discovery_query("private-query-keyword".to_owned(), 1, 5);

    let guard = collector.begin_acp_management();
    let rendered = format!("{guard:?}");
    assert!(
        !rendered.contains("private-query-keyword"),
        "the guard's Debug rendering must not publish recorded discovery queries: {rendered}"
    );
    drop(guard);
}

#[test]
fn acp_management_duration_sums_sub_millisecond_requests() {
    let collector = MetricsCollector::new();
    for _ in 0..4 {
        collector.record_acp_management_outcome(true, Duration::from_micros(400));
    }

    assert_eq!(
        collector.get_acp_extension_metrics().management_total_duration_ms,
        1,
        "four 400 us requests total 1.6 ms, so truncation must happen once at snapshot time"
    );
}
