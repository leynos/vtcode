//! Bounded metrics for the Lody ACP extension surface.
//!
//! The family is self-contained, so it lives here to keep `metrics/mod.rs`
//! navigable. Every public item is re-exported from the parent module, which
//! keeps the `diapason_core::metrics` paths stable for callers.

use serde::{Deserialize, Serialize};
use std::fmt::Write;
use std::time::Instant;

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
    pub usage_notifications_sent: u64,
    pub usage_notifications_failed: u64,
    pub initial_snapshots_published: u64,
    pub initial_snapshot_publish_failures: u64,
}

/// Tracks one Lody ACP management RPC from entry to completion.
///
/// The guard raises the in-flight gauge when it is created and lowers it
/// exactly once. Dropping it without calling [`finish`](Self::finish) records
/// a failure, so a cancelled or disconnected request cannot leave the gauge
/// elevated.
#[derive(Debug)]
pub struct AcpManagementGuard {
    metrics: MetricsCollector,
    started_at: Instant,
    finished: bool,
}

impl AcpManagementGuard {
    /// Records the request outcome and elapsed time, then releases the gauge.
    pub fn finish(mut self, succeeded: bool) {
        self.finished = true;
        self.metrics.record_acp_management_outcome(succeeded, self.elapsed_ms());
    }

    fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.started_at.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

impl Drop for AcpManagementGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.metrics.record_acp_management_outcome(false, self.elapsed_ms());
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
            metrics: self.clone(),
            started_at: Instant::now(),
            finished: false,
        }
    }

    /// Record one Lody ACP management RPC outcome and release its gauge slot.
    fn record_acp_management_outcome(&self, succeeded: bool, duration_ms: u64) {
        let mut metrics = self.acp_extension.lock();
        if succeeded {
            metrics.management_requests_succeeded = metrics.management_requests_succeeded.saturating_add(1);
        } else {
            metrics.management_requests_failed = metrics.management_requests_failed.saturating_add(1);
        }
        metrics.management_total_duration_ms = metrics.management_total_duration_ms.saturating_add(duration_ms);
        metrics.management_requests_in_flight = metrics.management_requests_in_flight.saturating_sub(1);
    }

    /// Record the outcome of one Lody ACP usage notification publication.
    pub fn record_acp_usage_notification(&self, succeeded: bool) {
        let mut metrics = self.acp_extension.lock();
        if succeeded {
            metrics.usage_notifications_sent = metrics.usage_notifications_sent.saturating_add(1);
        } else {
            metrics.usage_notifications_failed = metrics.usage_notifications_failed.saturating_add(1);
        }
    }

    /// Record snapshots queued by one attach-time initial publication pass.
    pub fn record_acp_initial_snapshots_published(&self, snapshots: usize) {
        let mut metrics = self.acp_extension.lock();
        let snapshots = u64::try_from(snapshots).unwrap_or(u64::MAX);
        metrics.initial_snapshots_published = metrics.initial_snapshots_published.saturating_add(snapshots);
    }

    /// Record an initial task snapshot that could not be queued.
    pub fn record_acp_initial_snapshot_publish_failure(&self) {
        let mut metrics = self.acp_extension.lock();
        metrics.initial_snapshot_publish_failures = metrics.initial_snapshot_publish_failures.saturating_add(1);
    }

    /// Get the bounded aggregate Lody ACP extension metrics snapshot.
    pub fn get_acp_extension_metrics(&self) -> AcpExtensionMetrics {
        self.acp_extension.lock().clone()
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
         # HELP diapason_acp_usage_notifications_sent_total Lody ACP usage notifications sent\n\
         # TYPE diapason_acp_usage_notifications_sent_total counter\n\
         diapason_acp_usage_notifications_sent_total {}\n\n\
         # HELP diapason_acp_usage_notifications_failed_total Lody ACP usage notifications that failed to send\n\
         # TYPE diapason_acp_usage_notifications_failed_total counter\n\
         diapason_acp_usage_notifications_failed_total {}\n\n\
         # HELP diapason_acp_initial_snapshots_published_total Lody ACP initial task snapshots published\n\
         # TYPE diapason_acp_initial_snapshots_published_total counter\n\
         diapason_acp_initial_snapshots_published_total {}\n\n\
         # HELP diapason_acp_initial_snapshot_publish_failures_total Lody ACP initial task snapshots that failed to publish\n\
         # TYPE diapason_acp_initial_snapshot_publish_failures_total counter\n\
         diapason_acp_initial_snapshot_publish_failures_total {}\n\n",
        metrics.management_requests_started,
        metrics.management_requests_succeeded,
        metrics.management_requests_failed,
        metrics.management_requests_in_flight,
        metrics.management_total_duration_ms,
        metrics.usage_notifications_sent,
        metrics.usage_notifications_failed,
        metrics.initial_snapshots_published,
        metrics.initial_snapshot_publish_failures,
    );
}

#[cfg(test)]
mod tests {
    use crate::metrics::MetricsCollector;

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
        collector.record_acp_initial_snapshots_published(2);
        collector.record_acp_initial_snapshot_publish_failure();

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
        # HELP diapason_acp_usage_notifications_sent_total Lody ACP usage notifications sent
        # TYPE diapason_acp_usage_notifications_sent_total counter
        diapason_acp_usage_notifications_sent_total 1
        # HELP diapason_acp_usage_notifications_failed_total Lody ACP usage notifications that failed to send
        # TYPE diapason_acp_usage_notifications_failed_total counter
        diapason_acp_usage_notifications_failed_total 1
        # HELP diapason_acp_initial_snapshots_published_total Lody ACP initial task snapshots published
        # TYPE diapason_acp_initial_snapshots_published_total counter
        diapason_acp_initial_snapshots_published_total 2
        # HELP diapason_acp_initial_snapshot_publish_failures_total Lody ACP initial task snapshots that failed to publish
        # TYPE diapason_acp_initial_snapshot_publish_failures_total counter
        diapason_acp_initial_snapshot_publish_failures_total 1
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
}
