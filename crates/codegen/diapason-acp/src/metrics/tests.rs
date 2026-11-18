//! Covers the compaction and publication counter arithmetic.
//!
//! The counters are the observable half of the ACP metrics boundary, so
//! these tests pin their classification, saturation, and snapshot
//! behaviour without starting an ACP connection.

use super::*;

/// Assert a failure outranks a partially compacted history.
#[test]
fn failure_takes_precedence_over_compaction_state() {
    assert_eq!(CompactionOutcome::for_result(true, true), CompactionOutcome::Failed);
    assert_eq!(CompactionOutcome::for_result(true, false), CompactionOutcome::Failed);
    assert_eq!(CompactionOutcome::for_result(false, true), CompactionOutcome::Compacted);
    assert_eq!(CompactionOutcome::for_result(false, false), CompactionOutcome::NoChange);
}

/// Assert each outcome has a distinct stable label.
#[test]
fn outcome_labels_are_closed_and_distinct() {
    let labels = [
        CompactionOutcome::Failed.as_str(),
        CompactionOutcome::Compacted.as_str(),
        CompactionOutcome::NoChange.as_str(),
    ];
    assert_eq!(labels, ["failed", "compacted", "no_change"]);
    let mut unique = labels.to_vec();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), labels.len());
}

/// Assert counters accumulate per outcome and track duration bounds.
#[test]
fn counters_accumulate_outcomes_and_durations() {
    let counters = CompactionCounters::new();
    counters.record_started();
    counters.record_finished(CompactionOutcome::Compacted, false, 40);
    counters.record_started();
    counters.record_finished(CompactionOutcome::NoChange, false, 10);
    counters.record_started();
    counters.record_finished(CompactionOutcome::Failed, true, 90);

    let snapshot = counters.snapshot();
    assert_eq!(snapshot.started, 3);
    assert_eq!(snapshot.compacted, 1);
    assert_eq!(snapshot.no_change, 1);
    assert_eq!(snapshot.failed, 1);
    assert_eq!(snapshot.cancelled, 1, "cancelled is a subset of failed");
    assert_eq!(snapshot.elapsed_ms_total, 140);
    assert_eq!(snapshot.elapsed_ms_max, 90);
}

/// Assert a fresh counter set reports zeros rather than stale values.
#[test]
fn fresh_counters_report_zero() {
    assert_eq!(CompactionCounters::new().snapshot(), CompactionMetrics::default());
}

/// Assert publication counters separate attempts from failures.
#[test]
fn publication_counters_track_attempts_and_failures() {
    let counters = PublicationCounters::new();
    counters.record(true);
    counters.record(false);
    counters.record(false);

    let snapshot = counters.snapshot();
    assert_eq!(snapshot.attempts, 3);
    assert_eq!(snapshot.failures, 2, "failures are a subset of attempts");
}

/// Assert a fresh publication counter set reports zeros.
#[test]
fn fresh_publication_counters_report_zero() {
    assert_eq!(PublicationCounters::new().snapshot(), PublicationMetrics::default());
}

/// Assert a snapshot never reports more failures than attempts.
///
/// The two counters are separate atomics, so a concurrent reader can
/// observe the failure increment before the attempt increment. The
/// snapshot clamps `attempts` to cover every observed failure so the
/// published subset invariant holds regardless of interleaving.
#[test]
fn publication_snapshot_never_reports_more_failures_than_attempts() {
    let counters = PublicationCounters::new();
    counters.failures.store(3, Ordering::Relaxed);
    counters.attempts.store(1, Ordering::Relaxed);

    let snapshot = counters.snapshot();
    assert_eq!(snapshot.failures, 3);
    assert_eq!(snapshot.attempts, 3, "attempts must be clamped to cover every observed failure");
}

/// Assert a compaction snapshot never reports fewer failures than
/// cancellations.
///
/// `record_finished` increments both counters for a cancelled attempt. A
/// concurrent reader can therefore observe the newer `cancelled` value
/// beside the older `failed` value. The snapshot clamps `failed` upward so
/// the published subset invariant holds regardless of interleaving.
#[test]
fn compaction_snapshot_never_reports_fewer_failures_than_cancellations() {
    let counters = CompactionCounters::new();
    counters.cancelled.store(3, Ordering::Relaxed);
    counters.failed.store(1, Ordering::Relaxed);

    let snapshot = counters.snapshot();
    assert_eq!(snapshot.cancelled, 3);
    assert_eq!(snapshot.failed, 3, "failed must be clamped to cover every observed cancellation");
}

/// Assert counter increments saturate rather than wrapping.
#[test]
fn counters_saturate_at_the_maximum() {
    let counter = AtomicU64::new(u64::MAX);
    saturating_add(&counter, 1);
    assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
}

/// Assert a recorder seeded near the ceiling saturates rather than wrapping.
///
/// A process that has been running long enough to reach these values must
/// keep reporting a usable snapshot, and the outcome it did not record must
/// stay where it was.
#[test]
fn counters_seeded_near_the_ceiling_saturate() {
    let counters = CompactionCounters::new();
    let start = u64::MAX - 1;
    counters.started.store(start, Ordering::Relaxed);
    counters.compacted.store(start, Ordering::Relaxed);
    counters.failed.store(start, Ordering::Relaxed);
    counters.cancelled.store(start, Ordering::Relaxed);
    counters.elapsed_ms_total.store(start, Ordering::Relaxed);
    counters.elapsed_ms_max.store(start, Ordering::Relaxed);

    counters.record_started();
    counters.record_finished(CompactionOutcome::Failed, true, 5);

    let snapshot = counters.snapshot();
    assert_eq!(snapshot.started, u64::MAX);
    assert_eq!(snapshot.compacted, start, "an unrecorded outcome leaves its counter alone");
    assert_eq!(snapshot.failed, u64::MAX);
    assert_eq!(snapshot.cancelled, u64::MAX);
    assert_eq!(snapshot.elapsed_ms_total, u64::MAX);
    assert_eq!(snapshot.elapsed_ms_max, start, "a short attempt cannot raise a saturated maximum");
}
