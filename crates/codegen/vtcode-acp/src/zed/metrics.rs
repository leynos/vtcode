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
