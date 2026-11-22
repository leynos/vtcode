//! Bounded metrics for the Lody ACP extension surface.
//!
//! The family is self-contained, so it lives here to keep `metrics/mod.rs`
//! navigable. Every public item is re-exported from the parent module, which
//! keeps the `diapason_core::metrics` paths stable for callers.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::fmt::{self, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{MetricsCollector, PrometheusMetricsSnapshot};

/// Bounded aggregate metrics for the Lody ACP extension surface.
///
/// The counters deliberately carry no session, task, model, or payload labels,
/// and the operation identity is bounded to the fixed management RPC set, so
/// the same snapshot is safe to expose through JSON and Prometheus.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcpExtensionMetrics {
    pub management_requests_started: u64,
    pub management_requests_succeeded: u64,
    pub management_requests_failed: u64,
    pub management_requests_in_flight: u64,
    pub management_total_duration_ms: u64,
    pub usage_notifications_enqueued: u64,
    pub usage_notifications_failed: u64,
    /// Initial task snapshots accepted onto the lifecycle queue.
    ///
    /// The forwarder can still discard a queued snapshot as stale, so this
    /// counts queue acceptance rather than client delivery.
    pub initial_snapshots_enqueued: u64,
    /// Initial task snapshots the lifecycle queue rejected.
    pub initial_snapshots_rejected: u64,
}

/// Tracks one Lody ACP management RPC from entry to completion.
///
/// The guard raises the in-flight gauge when it is created and lowers it
/// exactly once. Dropping it without calling [`finish`](Self::finish) records
/// a failure, so a cancelled or disconnected request cannot leave the gauge
/// elevated.
pub struct AcpManagementGuard {
    /// The ACP extension state this guard accounts against.
    ///
    /// The guard is held across a handler's awaits, so it retains the one
    /// shared state handle it uses rather than a clone of the whole collector
    /// and every metric family the collector owns.
    state: Arc<Mutex<AcpExtensionState>>,
    started_at: Instant,
    finished: bool,
}

impl fmt::Debug for AcpManagementGuard {
    // `MetricsCollector`'s derived rendering walks every family, including the
    // discovery records that hold user-supplied query keywords, so the guard
    // renders only its own fields.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcpManagementGuard")
            .field("started_at", &self.started_at)
            .field("finished", &self.finished)
            .finish_non_exhaustive()
    }
}

impl AcpManagementGuard {
    /// Records the request outcome and elapsed time, then releases the gauge.
    pub fn finish(mut self, succeeded: bool) {
        self.finished = true;
        self.state
            .lock()
            .record_management_outcome(succeeded, self.started_at.elapsed());
    }
}

impl Drop for AcpManagementGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.state.lock().record_management_outcome(false, self.started_at.elapsed());
        }
    }
}

/// Collector-local ACP extension counters.
///
/// The management duration is accumulated as a [`Duration`] so that
/// sub-millisecond requests are not truncated before they are summed. The
/// public [`AcpExtensionMetrics`] snapshot converts the total once, when it is
/// taken.
#[derive(Debug, Clone, Default)]
pub(super) struct AcpExtensionState {
    management_requests_started: u64,
    management_requests_succeeded: u64,
    management_requests_failed: u64,
    management_requests_in_flight: u64,
    management_total_duration: Duration,
    usage_notifications_enqueued: u64,
    usage_notifications_failed: u64,
    initial_snapshots_enqueued: u64,
    initial_snapshots_rejected: u64,
}

impl AcpExtensionState {
    /// Record one management outcome and release its in-flight slot.
    ///
    /// This is the single accounting point shared by the guard's `finish` and
    /// unfinished `Drop` paths and by the collector's direct-call seam, so a
    /// request can record at most one outcome from either caller.
    fn record_management_outcome(&mut self, succeeded: bool, duration: Duration) {
        if succeeded {
            self.management_requests_succeeded = self.management_requests_succeeded.saturating_add(1);
        } else {
            self.management_requests_failed = self.management_requests_failed.saturating_add(1);
        }
        self.management_total_duration = self.management_total_duration.saturating_add(duration);
        self.management_requests_in_flight = self.management_requests_in_flight.saturating_sub(1);
    }

    /// Convert the accumulated counters into the bounded public snapshot.
    fn snapshot(&self) -> AcpExtensionMetrics {
        AcpExtensionMetrics {
            management_requests_started: self.management_requests_started,
            management_requests_succeeded: self.management_requests_succeeded,
            management_requests_failed: self.management_requests_failed,
            management_requests_in_flight: self.management_requests_in_flight,
            management_total_duration_ms: u64::try_from(self.management_total_duration.as_millis()).unwrap_or(u64::MAX),
            usage_notifications_enqueued: self.usage_notifications_enqueued,
            usage_notifications_failed: self.usage_notifications_failed,
            initial_snapshots_enqueued: self.initial_snapshots_enqueued,
            initial_snapshots_rejected: self.initial_snapshots_rejected,
        }
    }
}

impl MetricsCollector {
    /// Begin tracking one Lody ACP management RPC.
    ///
    /// All three management RPCs are counted, including the `list` and
    /// `output` queries: the counters measure how often a session uses the
    /// management surface, not how many requests mutate state. The counting
    /// happens at the adapter's dispatch boundary, so a query helper called
    /// directly records nothing.
    ///
    /// The returned guard raises the in-flight gauge and lowers it exactly
    /// once. A request that is dropped before
    /// [`AcpManagementGuard::finish`] — for example because the client
    /// disconnected while the handler awaited — is recorded as a failure, so
    /// the gauge cannot stay elevated.
    ///
    /// `management_requests_started` is incremented before any outcome is
    /// recorded, so the counters relate as
    /// `started = succeeded + failed + in_flight`: the equality
    /// `started = succeeded + failed` holds only at rest, once every request
    /// that was started has recorded its outcome. A gap between them with
    /// `in_flight` at zero therefore means a request has no recorded outcome.
    pub fn begin_acp_management(&self) -> AcpManagementGuard {
        let mut metrics = self.acp_extension.lock();
        metrics.management_requests_started = metrics.management_requests_started.saturating_add(1);
        metrics.management_requests_in_flight = metrics.management_requests_in_flight.saturating_add(1);
        drop(metrics);
        AcpManagementGuard {
            state: Arc::clone(&self.acp_extension),
            started_at: Instant::now(),
            finished: false,
        }
    }

    /// Record one Lody ACP management RPC outcome and release its gauge slot.
    ///
    /// The guard accounts through the same shared state; this collector-level
    /// entry point exists only for tests that record an outcome directly.
    #[cfg(test)]
    fn record_acp_management_outcome(&self, succeeded: bool, duration: Duration) {
        self.acp_extension.lock().record_management_outcome(succeeded, duration);
    }

    /// Record one attempt to enqueue a Lody ACP usage notification.
    ///
    /// Success means the notification entered the connection's outgoing queue,
    /// not that the client has received it.
    pub fn record_acp_usage_notification(&self, succeeded: bool) {
        let mut metrics = self.acp_extension.lock();
        if succeeded {
            metrics.usage_notifications_enqueued = metrics.usage_notifications_enqueued.saturating_add(1);
        } else {
            metrics.usage_notifications_failed = metrics.usage_notifications_failed.saturating_add(1);
        }
    }

    /// Record snapshots accepted onto the lifecycle queue by one attach-time
    /// initial publication pass.
    ///
    /// The forwarder may discard a queued snapshot as stale, so this measures
    /// queue acceptance, not that the client received the snapshot.
    pub fn record_acp_initial_snapshots_enqueued(&self, snapshots: usize) {
        let mut metrics = self.acp_extension.lock();
        let snapshots = u64::try_from(snapshots).unwrap_or(u64::MAX);
        metrics.initial_snapshots_enqueued = metrics.initial_snapshots_enqueued.saturating_add(snapshots);
    }

    /// Record an initial task snapshot the lifecycle queue rejected.
    pub fn record_acp_initial_snapshot_rejected(&self) {
        let mut metrics = self.acp_extension.lock();
        metrics.initial_snapshots_rejected = metrics.initial_snapshots_rejected.saturating_add(1);
    }

    /// Get the bounded aggregate Lody ACP extension metrics snapshot.
    pub fn get_acp_extension_metrics(&self) -> AcpExtensionMetrics {
        self.acp_extension.lock().snapshot()
    }
}

impl PrometheusMetricsSnapshot {
    pub(super) fn append_acp_extension(&self, output: &mut String) {
        append_acp_extension_prometheus(output, &self.acp_extension);
    }
}

fn append_acp_extension_prometheus(output: &mut String, metrics: &AcpExtensionMetrics) {
    let _ = write!(
        output,
        "# HELP diapason_acp_management_requests_started_total Lody ACP management requests started (list, cancel and output)\n\
         # TYPE diapason_acp_management_requests_started_total counter\n\
         diapason_acp_management_requests_started_total {}\n\n\
         # HELP diapason_acp_management_requests_succeeded_total Lody ACP management requests that succeeded\n\
         # TYPE diapason_acp_management_requests_succeeded_total counter\n\
         diapason_acp_management_requests_succeeded_total {}\n\n\
         # HELP diapason_acp_management_requests_failed_total Lody ACP management requests that failed\n\
         # TYPE diapason_acp_management_requests_failed_total counter\n\
         diapason_acp_management_requests_failed_total {}\n\n\
         # HELP diapason_acp_management_requests_in_flight Lody ACP management requests currently running\n\
         # TYPE diapason_acp_management_requests_in_flight gauge\n\
         diapason_acp_management_requests_in_flight {}\n\n\
         # HELP diapason_acp_management_duration_ms_total Total time spent serving Lody ACP management requests\n\
         # TYPE diapason_acp_management_duration_ms_total counter\n\
         diapason_acp_management_duration_ms_total {}\n\n\
         # HELP diapason_acp_usage_notifications_enqueued_total Lody ACP usage notifications enqueued for delivery\n\
         # TYPE diapason_acp_usage_notifications_enqueued_total counter\n\
         diapason_acp_usage_notifications_enqueued_total {}\n\n\
         # HELP diapason_acp_usage_notifications_failed_total Lody ACP usage notification publication failures, including serialization before enqueue\n\
         # TYPE diapason_acp_usage_notifications_failed_total counter\n\
         diapason_acp_usage_notifications_failed_total {}\n\n\
         # HELP diapason_acp_initial_snapshots_enqueued_total Lody ACP initial task snapshots accepted onto the lifecycle queue\n\
         # TYPE diapason_acp_initial_snapshots_enqueued_total counter\n\
         diapason_acp_initial_snapshots_enqueued_total {}\n\n\
         # HELP diapason_acp_initial_snapshots_rejected_total Lody ACP initial task snapshots rejected by the lifecycle queue\n\
         # TYPE diapason_acp_initial_snapshots_rejected_total counter\n\
         diapason_acp_initial_snapshots_rejected_total {}\n\n",
        metrics.management_requests_started,
        metrics.management_requests_succeeded,
        metrics.management_requests_failed,
        metrics.management_requests_in_flight,
        metrics.management_total_duration_ms,
        metrics.usage_notifications_enqueued,
        metrics.usage_notifications_failed,
        metrics.initial_snapshots_enqueued,
        metrics.initial_snapshots_rejected,
    );
}

#[cfg(test)]
mod tests {
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
        diapason_acp_usage_notifications_enqueued_total 1
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
}
