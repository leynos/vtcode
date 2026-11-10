//! Metrics-related accessors for ToolRegistry.

use super::ToolRegistry;

impl ToolRegistry {
    /// Return the shared metrics collector for this registry instance.
    pub fn metrics_collector(&self) -> std::sync::Arc<crate::metrics::MetricsCollector> {
        self.metrics.clone()
    }

    /// Install `metrics` as this registry's shared collector.
    ///
    /// ACP workspace registries share the agent-wide collector so recovery
    /// counters recorded by any workspace appear in the agent's snapshot.
    /// Components that captured the previous collector at construction keep
    /// it; execution paths read the collector through
    /// [`Self::metrics_collector`] and therefore observe the replacement.
    pub fn set_metrics_collector(&mut self, metrics: std::sync::Arc<crate::metrics::MetricsCollector>) {
        self.metrics = metrics;
    }

    /// Get total tool calls made in current session (for observability).
    pub fn tool_call_count(&self) -> u64 {
        self.tool_call_counter.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get total PTY poll iterations (for CPU monitoring).
    pub fn pty_poll_count(&self) -> u64 {
        self.pty_poll_counter.load(std::sync::atomic::Ordering::Relaxed)
    }
}
