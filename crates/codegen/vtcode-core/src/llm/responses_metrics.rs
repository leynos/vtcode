//! Provider-side adapter bridging Responses stream counters to the core
//! metrics port.
//!
//! The concrete provider type conversion lives here rather than in
//! `crate::metrics`, so the metrics module depends only on its own
//! [`ResponsesMetricsSource`] port and never names a provider API.
//!
//! [`ResponsesMetricsSource`]: crate::metrics::ResponsesMetricsSource

use crate::metrics::{ResponsesMetricsSource, ResponsesStreamMetrics, ResponsesStreamOutcomeMetric};

/// Samples the process-wide Responses stream and WebSocket recovery counters.
///
/// Every call reads the live atomics, so a snapshot reflects the counters at
/// read time and is never cached. The counters are process-wide: all collectors
/// and all sessions observe the same values, and reading them does not reset or
/// mutate anything. Labels come from closed enums in the provider layer, so the
/// series count stays bounded.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessResponsesMetricsSource;

impl ProcessResponsesMetricsSource {
    /// Create a source that reads the process-wide Responses counters.
    pub fn new() -> Self {
        Self
    }
}

impl ResponsesMetricsSource for ProcessResponsesMetricsSource {
    fn snapshot(&self) -> ResponsesStreamMetrics {
        let stream = vtcode_llm::providers::shared::responses_stream_metrics();
        let websocket = vtcode_llm::providers::shared::responses_websocket_metrics();
        ResponsesStreamMetrics {
            // Every provider outcome is mapped, so a new terminal outcome cannot
            // be silently dropped from the exported metrics.
            outcomes: stream
                .into_iter()
                .map(|metric| ResponsesStreamOutcomeMetric {
                    path: metric.path.as_str().to_string(),
                    outcome: metric.outcome.as_str().to_string(),
                    total: metric.total,
                })
                .collect(),
            websocket_replay_prohibited_total: websocket.replay_prohibited_total,
            websocket_http_fallback_total: websocket.http_fallback_total,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Mapping coverage for the Responses metrics adapter.
    //!
    //! The tests pin the port contract: every provider outcome reaches the
    //! snapshot with its stable label, and the recovery counters stay monotonic.

    use super::ProcessResponsesMetricsSource;
    use crate::metrics::{ResponsesMetricsSource, ResponsesStreamMetrics};

    /// Every provider `(path, outcome)` pair must reach the port exactly once.
    #[test]
    fn every_provider_outcome_is_mapped_to_the_snapshot() {
        let provider = vtcode_llm::providers::shared::responses_stream_metrics();
        let snapshot = ProcessResponsesMetricsSource::new().snapshot();

        assert_eq!(snapshot.outcomes.len(), provider.len(), "no outcome may be dropped");
        for (provider_metric, mapped) in provider.iter().zip(&snapshot.outcomes) {
            assert_eq!(mapped.path, provider_metric.path.as_str(), "path label must map");
            assert_eq!(mapped.outcome, provider_metric.outcome.as_str(), "outcome label must map");
        }
        assert_eq!(snapshot.outcomes.len(), 22, "two stream paths times eleven terminal outcomes");
    }

    /// Recovery counters must be read from the provider counters, not zeroed.
    #[test]
    fn websocket_recovery_counters_map_from_the_provider() {
        let before = vtcode_llm::providers::shared::responses_websocket_metrics();
        let snapshot = ProcessResponsesMetricsSource::new().snapshot();
        let after = vtcode_llm::providers::shared::responses_websocket_metrics();

        // Concurrent streams may advance the counters between reads, so assert
        // monotonic containment rather than an exact equality.
        assert!(snapshot.websocket_replay_prohibited_total >= before.replay_prohibited_total);
        assert!(snapshot.websocket_replay_prohibited_total <= after.replay_prohibited_total);
        assert!(snapshot.websocket_http_fallback_total >= before.http_fallback_total);
        assert!(snapshot.websocket_http_fallback_total <= after.http_fallback_total);
    }

    /// The snapshot is a pure value with a bounded, closed label set.
    #[test]
    fn snapshot_serializes_closed_labels_only() {
        let snapshot = ResponsesStreamMetrics::default();
        let json = serde_json::to_value(&snapshot).expect("snapshot must serialize");
        let object = json.as_object().expect("snapshot serializes as an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "outcomes",
                "websocket_http_fallback_total",
                "websocket_replay_prohibited_total"
            ],
            "the snapshot must not introduce provider or session labels"
        );
    }
}
