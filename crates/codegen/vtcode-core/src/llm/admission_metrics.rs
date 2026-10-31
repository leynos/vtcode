//! Provider-side adapter bridging custom-provider admission counters to the
//! core metrics port.
//!
//! The concrete provider type conversion lives here rather than in
//! `crate::metrics`, so the metrics module depends only on its own
//! [`ProviderAdmissionMetricsSource`] port and never names a provider API.
//!
//! [`ProviderAdmissionMetricsSource`]: crate::metrics::ProviderAdmissionMetricsSource

use crate::metrics::{ProviderAdmissionMetricsSource, ProviderAdmissionSnapshot};

/// Samples the process-wide custom-provider admission counters.
///
/// Every call reads the live atomics, so a snapshot reflects the counters at
/// read time and is never cached. The counters are process-wide: all collectors
/// and all sessions observe the same values, and reading them does not reset or
/// mutate anything.
///
/// Consistency: the underlying counters are loaded independently with relaxed
/// ordering, so one snapshot may combine values from slightly different
/// instants while admission activity is in progress. The gauges (`queue_depth`,
/// `in_flight`) are current counts; the remaining fields are cumulative.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessProviderAdmissionMetricsSource;

impl ProcessProviderAdmissionMetricsSource {
    /// Create a source that reads the process-wide admission counters.
    pub fn new() -> Self {
        Self
    }
}

impl ProviderAdmissionMetricsSource for ProcessProviderAdmissionMetricsSource {
    fn snapshot(&self) -> ProviderAdmissionSnapshot {
        // Every admission counter is mapped, so a new provider counter cannot be
        // silently dropped from the exported metrics.
        let admission = vtcode_llm::providers::custom_provider_admission_metrics();
        ProviderAdmissionSnapshot {
            queue_depth: admission.queue_depth,
            in_flight: admission.in_flight,
            queue_waits: admission.queue_waits,
            queue_wait_ms: admission.queue_wait_ms,
            queue_timeouts: admission.queue_timeouts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::ProcessProviderAdmissionMetricsSource;
    use crate::metrics::{ProviderAdmissionMetricsSource, ProviderAdmissionSnapshot};

    /// The adapter must carry every provider admission counter across the port.
    /// A dropped field would silently export a stale zero, so each provider
    /// counter is asserted against its own snapshot field by name.
    #[test]
    fn every_provider_admission_counter_is_mapped_to_the_snapshot() {
        let provider = vtcode_llm::providers::custom_provider_admission_metrics();
        let snapshot = ProcessProviderAdmissionMetricsSource::new().snapshot();

        assert_eq!(snapshot.queue_depth, provider.queue_depth, "queue_depth must map");
        assert_eq!(snapshot.in_flight, provider.in_flight, "in_flight must map");
        assert_eq!(snapshot.queue_waits, provider.queue_waits, "queue_waits must map");
        assert_eq!(snapshot.queue_wait_ms, provider.queue_wait_ms, "queue_wait_ms must map");
        assert_eq!(snapshot.queue_timeouts, provider.queue_timeouts, "queue_timeouts must map");
    }

    /// The snapshot type is a pure value so consumers can compare, clone and
    /// serialize it; assert the shape carries exactly the five counters.
    #[test]
    fn snapshot_exposes_only_the_bounded_admission_counters() {
        let snapshot = ProviderAdmissionSnapshot {
            queue_depth: 1,
            in_flight: 2,
            queue_waits: 3,
            queue_wait_ms: 4,
            queue_timeouts: 5,
        };

        let json = serde_json::to_value(snapshot).expect("snapshot must serialize");
        let object = json.as_object().expect("snapshot serializes as an object");
        let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "in_flight",
                "queue_depth",
                "queue_timeouts",
                "queue_wait_ms",
                "queue_waits"
            ],
            "the snapshot must not introduce provider or session labels"
        );
    }
}
