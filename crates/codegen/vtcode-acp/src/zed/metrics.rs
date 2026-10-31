//! Translates concrete MCP startup callbacks into core's service metrics port.

use std::sync::Arc;
use vtcode_core::mcp::{McpClientStartupOutcome, McpProviderStartupOutcome, McpStartupObserver};
use vtcode_core::metrics::{
    MetricsCollector, ServiceClientStartupOutcome, ServiceProviderStartupOutcome, ServiceStartupMetricsObserver,
};

pub(super) struct AcpMcpStartupMetricsObserver(Arc<MetricsCollector>);

impl AcpMcpStartupMetricsObserver {
    pub(super) fn new(metrics: Arc<MetricsCollector>) -> Self {
        Self(metrics)
    }
}

impl McpStartupObserver for AcpMcpStartupMetricsObserver {
    fn client_startup_started(&self) {
        self.0.client_startup_started();
    }

    fn client_startup_finished(
        &self,
        outcome: McpClientStartupOutcome,
        previous_active_provider_count: u64,
        active_provider_count: u64,
        duration_ms: u64,
    ) {
        let outcome = match outcome {
            McpClientStartupOutcome::Succeeded => ServiceClientStartupOutcome::Succeeded,
            McpClientStartupOutcome::Partial => ServiceClientStartupOutcome::Partial,
            McpClientStartupOutcome::Failed => ServiceClientStartupOutcome::Failed,
            McpClientStartupOutcome::Disabled => ServiceClientStartupOutcome::Disabled,
            McpClientStartupOutcome::Cancelled => ServiceClientStartupOutcome::Cancelled,
        };
        self.0
            .client_startup_finished(outcome, previous_active_provider_count, active_provider_count, duration_ms);
    }

    fn provider_startup_started(&self) {
        self.0.provider_startup_started();
    }

    fn provider_startup_finished(&self, outcome: McpProviderStartupOutcome, duration_ms: u64) {
        let outcome = match outcome {
            McpProviderStartupOutcome::Succeeded => ServiceProviderStartupOutcome::Succeeded,
            McpProviderStartupOutcome::ConnectionFailed => ServiceProviderStartupOutcome::ConnectionFailed,
            McpProviderStartupOutcome::ConnectionTimedOut => ServiceProviderStartupOutcome::ConnectionTimedOut,
            McpProviderStartupOutcome::InitializationFailed => ServiceProviderStartupOutcome::InitializationFailed,
            McpProviderStartupOutcome::InitializationTimedOut => ServiceProviderStartupOutcome::InitializationTimedOut,
            McpProviderStartupOutcome::OtherFailed => ServiceProviderStartupOutcome::OtherFailed,
            McpProviderStartupOutcome::Cancelled => ServiceProviderStartupOutcome::Cancelled,
        };
        self.0.provider_startup_finished(outcome, duration_ms);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use vtcode_core::metrics::ServiceStartupMetrics;

    /// Every client outcome must be recorded in its own bucket and advance
    /// both the in-flight and duration aggregates; a mis-mapped or missing arm
    /// fails this assertion.
    #[rstest]
    #[case::succeeded(McpClientStartupOutcome::Succeeded, |m: &ServiceStartupMetrics| m.client_attempts_succeeded)]
    #[case::partial(McpClientStartupOutcome::Partial, |m: &ServiceStartupMetrics| m.client_attempts_partial)]
    #[case::failed(McpClientStartupOutcome::Failed, |m: &ServiceStartupMetrics| m.client_attempts_failed)]
    #[case::disabled(McpClientStartupOutcome::Disabled, |m: &ServiceStartupMetrics| m.client_attempts_disabled)]
    #[case::cancelled(McpClientStartupOutcome::Cancelled, |m: &ServiceStartupMetrics| m.client_attempts_cancelled)]
    fn client_outcomes_map_to_their_own_bucket(
        #[case] outcome: McpClientStartupOutcome,
        #[case] bucket: fn(&ServiceStartupMetrics) -> u64,
    ) {
        let collector = Arc::new(MetricsCollector::new());
        let observer = AcpMcpStartupMetricsObserver::new(Arc::clone(&collector));

        observer.client_startup_started();
        observer.client_startup_finished(outcome, 0, 3, 42);

        let metrics = collector.get_service_startup_metrics();
        assert_eq!(bucket(&metrics), 1, "outcome {outcome:?} must land in its own bucket");
        assert_eq!(metrics.client_attempts_started, 1);
        assert_eq!(metrics.client_attempts_in_flight, 0, "completion must clear in-flight");
        assert_eq!(metrics.total_client_duration_ms, 42);
        assert_eq!(metrics.active_providers, 3, "active provider count must be recorded");
    }

    /// Provider failures are split by cause, not collapsed into one counter.
    #[rstest]
    #[case::succeeded(McpProviderStartupOutcome::Succeeded, |m: &ServiceStartupMetrics| m.provider_attempts_succeeded)]
    #[case::other_failed(McpProviderStartupOutcome::OtherFailed, |m: &ServiceStartupMetrics| m.provider_attempts_failed)]
    #[case::cancelled(McpProviderStartupOutcome::Cancelled, |m: &ServiceStartupMetrics| m.provider_attempts_cancelled)]
    #[case::connection_failed(McpProviderStartupOutcome::ConnectionFailed, |m: &ServiceStartupMetrics| m.provider_connection_failures)]
    #[case::connection_timed_out(McpProviderStartupOutcome::ConnectionTimedOut, |m: &ServiceStartupMetrics| m.provider_connection_timeouts)]
    #[case::initialization_failed(McpProviderStartupOutcome::InitializationFailed, |m: &ServiceStartupMetrics| m.provider_initialization_failures)]
    #[case::initialization_timed_out(McpProviderStartupOutcome::InitializationTimedOut, |m: &ServiceStartupMetrics| m.provider_initialization_timeouts)]
    fn provider_outcomes_map_to_their_own_bucket(
        #[case] outcome: McpProviderStartupOutcome,
        #[case] bucket: fn(&ServiceStartupMetrics) -> u64,
    ) {
        let collector = Arc::new(MetricsCollector::new());
        let observer = AcpMcpStartupMetricsObserver::new(Arc::clone(&collector));

        observer.provider_startup_started();
        observer.provider_startup_finished(outcome, 17);

        let metrics = collector.get_service_startup_metrics();
        assert_eq!(bucket(&metrics), 1, "outcome {outcome:?} must land in its own bucket");
        assert_eq!(metrics.provider_attempts_started, 1);
        assert_eq!(metrics.provider_attempts_in_flight, 0, "completion must clear in-flight");
        assert_eq!(metrics.total_provider_duration_ms, 17);
    }
}
