//! Unit tests for provider-request metrics and admission sampling.

use std::sync::atomic::Ordering;

use super::prometheus_helpers::append_custom_provider_admission_prometheus;
use super::*;

fn collector_with_provider_attempts() -> MetricsCollector {
    // Pin admission sampling to zeros so these tests assert only this
    // collector's own attempt accounting; live process-wide counters are
    // covered by the dedicated admission tests.
    let collector = MetricsCollector::with_admission_metrics_source(Arc::new(ZeroProviderAdmissionMetricsSource));
    collector.record_provider_attempt_started();
    collector.record_provider_attempt_failed(12, false, Some(3));
    collector.record_provider_retry_scheduled();
    collector.record_provider_attempt_finished();
    collector.record_provider_attempt_started();
    collector.record_provider_attempt_succeeded(34, Some(7));
    collector.record_provider_attempt_finished();
    collector
}

#[test]
fn provider_request_metrics_record_bounded_aggregate_counts_and_timings() {
    let collector = collector_with_provider_attempts();
    let metrics = collector.get_provider_request_metrics();
    let expected = ProviderRequestMetrics {
        attempts_started: 2,
        attempts_succeeded: 1,
        attempts_failed: 1,
        retries_scheduled: 1,
        total_duration_ms: 46,
        first_output_samples: 2,
        total_time_to_first_output_ms: 10,
        ..ProviderRequestMetrics::default()
    };

    assert_eq!(metrics, expected);
}

#[test]
fn provider_request_prometheus_exports_aggregate_attempt_samples() {
    let collector = collector_with_provider_attempts();
    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_provider_attempts_started_total 2"));
    assert!(prometheus.contains("diapason_provider_attempts_in_flight 0"));
    assert!(prometheus.contains("diapason_provider_time_to_first_output_ms_total 10"));
}

#[test]
fn provider_request_prometheus_exports_admission_samples() {
    let collector = collector_with_provider_attempts();
    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_depth"));
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_waits_total"));
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_timeouts_total"));
}

/// Fake admission source whose values change between reads, so a collector
/// that caches the sample or ignores the injected port fails loudly.
#[derive(Debug)]
struct ChangingAdmissionSource {
    reads: std::sync::Mutex<Vec<ProviderAdmissionSnapshot>>,
    next: std::sync::atomic::AtomicUsize,
}

impl ChangingAdmissionSource {
    fn new(samples: [ProviderAdmissionSnapshot; 2]) -> Self {
        Self {
            reads: std::sync::Mutex::new(samples.to_vec()),
            next: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

impl ProviderAdmissionMetricsSource for ChangingAdmissionSource {
    fn snapshot(&self) -> ProviderAdmissionSnapshot {
        let index = self.next.fetch_add(1, Ordering::SeqCst);
        let reads = self.reads.lock().expect("admission sample lock");
        reads[index.min(reads.len() - 1)]
    }
}

fn sample_source(samples: [ProviderAdmissionSnapshot; 2]) -> Arc<ChangingAdmissionSource> {
    Arc::new(ChangingAdmissionSource::new(samples))
}

#[test]
fn admission_metrics_are_sampled_from_the_injected_source_at_read_time() {
    let source = sample_source([
        ProviderAdmissionSnapshot {
            queue_depth: 3,
            in_flight: 2,
            queue_waits: 11,
            queue_wait_ms: 700,
            queue_timeouts: 1,
        },
        ProviderAdmissionSnapshot {
            queue_depth: 0,
            in_flight: 5,
            queue_waits: 19,
            queue_wait_ms: 1_400,
            queue_timeouts: 4,
        },
    ]);
    let collector = MetricsCollector::with_admission_metrics_source(source);

    let first = collector.get_provider_request_metrics();
    assert_eq!(first.custom_provider_queue_depth, 3);
    assert_eq!(first.custom_provider_in_flight, 2);
    assert_eq!(first.custom_provider_queue_waits, 11);
    assert_eq!(first.custom_provider_queue_wait_ms, 700);
    assert_eq!(first.custom_provider_queue_timeouts, 1);

    // The second read must observe the source's new values: sampling is live,
    // never a value captured at construction.
    let second = collector.get_provider_request_metrics();
    assert_eq!(second.custom_provider_queue_depth, 0);
    assert_eq!(second.custom_provider_in_flight, 5);
    assert_eq!(second.custom_provider_queue_waits, 19);
    assert_eq!(second.custom_provider_queue_wait_ms, 1_400);
    assert_eq!(second.custom_provider_queue_timeouts, 4);
    assert_ne!(first, second, "the injected source must be re-read on every call");
}

#[test]
fn json_export_carries_the_supplied_admission_values() {
    let source = sample_source([
        ProviderAdmissionSnapshot {
            queue_depth: 7,
            in_flight: 4,
            queue_waits: 31,
            queue_wait_ms: 2_500,
            queue_timeouts: 2,
        },
        ProviderAdmissionSnapshot::default(),
    ]);
    let collector = MetricsCollector::with_admission_metrics_source(source);

    let json = collector.export_json().expect("metrics JSON should serialize");
    let provider_requests = json
        .get("provider_requests")
        .expect("the summary must expose provider_requests");

    assert_eq!(provider_requests["custom_provider_queue_depth"], 7);
    assert_eq!(provider_requests["custom_provider_in_flight"], 4);
    assert_eq!(provider_requests["custom_provider_queue_waits"], 31);
    assert_eq!(provider_requests["custom_provider_queue_wait_ms"], 2_500);
    assert_eq!(provider_requests["custom_provider_queue_timeouts"], 2);
}

#[test]
fn prometheus_export_carries_the_supplied_admission_values() {
    let source = sample_source([
        ProviderAdmissionSnapshot {
            queue_depth: 7,
            in_flight: 4,
            queue_waits: 31,
            queue_wait_ms: 2_500,
            queue_timeouts: 2,
        },
        ProviderAdmissionSnapshot::default(),
    ]);
    let collector = MetricsCollector::with_admission_metrics_source(source);

    let prometheus = collector.export_prometheus();
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_depth 7"), "{prometheus}");
    assert!(prometheus.contains("diapason_custom_provider_admission_in_flight 4"), "{prometheus}");
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_waits_total 31"), "{prometheus}");
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_wait_ms_total 2500"), "{prometheus}");
    assert!(prometheus.contains("diapason_custom_provider_admission_queue_timeouts_total 2"), "{prometheus}");
}

#[test]
fn provider_request_prometheus_omits_provider_name_labels() {
    let collector = collector_with_provider_attempts();
    let prometheus = collector.export_prometheus();
    assert!(!prometheus.contains("provider=\""));
}

#[test]
fn custom_provider_admission_prometheus_uses_supplied_snapshot_values() {
    let metrics = ProviderRequestMetrics {
        custom_provider_queue_depth: 2,
        custom_provider_in_flight: 3,
        custom_provider_queue_waits: 5,
        custom_provider_queue_wait_ms: 7,
        custom_provider_queue_timeouts: 11,
        ..ProviderRequestMetrics::default()
    };
    let mut prometheus = String::new();

    append_custom_provider_admission_prometheus(&mut prometheus, &metrics);

    let samples: Vec<_> = prometheus
        .lines()
        .filter(|line| !line.starts_with('#') && !line.is_empty())
        .collect();
    assert_eq!(
        samples,
        vec![
            "diapason_custom_provider_admission_queue_depth 2",
            "diapason_custom_provider_admission_in_flight 3",
            "diapason_custom_provider_admission_queue_waits_total 5",
            "diapason_custom_provider_admission_queue_wait_ms_total 7",
            "diapason_custom_provider_admission_queue_timeouts_total 11",
        ]
    );
}
