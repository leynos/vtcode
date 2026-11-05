//! Bounded, process-local metrics for the ACP bridge.
//!
//! Counters are cumulative and never reset. Their cardinality is fixed by
//! construction: compaction outcomes come from [`CompactionOutcome`],
//! publication outcomes are the two states of a send, and no session, provider,
//! model, path, or payload value is ever recorded as a label.

use std::sync::atomic::{AtomicU64, Ordering};

/// Terminal outcome of one automatic compaction attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// The attempt failed; the reason is a separate closed-set category.
    Failed,
    /// The attempt reduced the history and committed the result.
    Compacted,
    /// The attempt finished inside the admission budget without compacting.
    NoChange,
}

impl CompactionOutcome {
    /// Classify a finished attempt from its failure and compaction state.
    ///
    /// A failure takes precedence: a partially compacted history that was not
    /// committed is still a failed attempt.
    pub(crate) const fn for_result(failed: bool, compacted: bool) -> Self {
        match (failed, compacted) {
            (true, _) => Self::Failed,
            (false, true) => Self::Compacted,
            (false, false) => Self::NoChange,
        }
    }

    /// Return the stable label used by spans, logs, and counters.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Failed => "failed",
            Self::Compacted => "compacted",
            Self::NoChange => "no_change",
        }
    }
}

/// Snapshot of the cumulative automatic-compaction counters.
///
/// `cancelled` is a subset of `failed`. Each field is loaded independently
/// with relaxed ordering, so concurrent updates can make the values reflect
/// slightly different instants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompactionMetrics {
    /// Attempts that passed the admission decision and started.
    pub started: u64,
    /// Attempts that committed a compacted history.
    pub compacted: u64,
    /// Attempts that finished without compacting.
    pub no_change: u64,
    /// Attempts that failed for any category.
    pub failed: u64,
    /// Attempts whose failure category was `cancelled`; included in `failed`.
    pub cancelled: u64,
    /// Sum of terminal attempt durations in milliseconds.
    pub elapsed_ms_total: u64,
    /// Longest terminal attempt duration in milliseconds.
    pub elapsed_ms_max: u64,
}

/// Cumulative counters behind [`compaction_metrics`].
struct CompactionCounters {
    started: AtomicU64,
    compacted: AtomicU64,
    no_change: AtomicU64,
    failed: AtomicU64,
    cancelled: AtomicU64,
    elapsed_ms_total: AtomicU64,
    elapsed_ms_max: AtomicU64,
}

impl CompactionCounters {
    const fn new() -> Self {
        Self {
            started: AtomicU64::new(0),
            compacted: AtomicU64::new(0),
            no_change: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            elapsed_ms_total: AtomicU64::new(0),
            elapsed_ms_max: AtomicU64::new(0),
        }
    }

    fn record_started(&self) {
        saturating_add(&self.started, 1);
    }

    fn record_finished(&self, outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64) {
        match outcome {
            CompactionOutcome::Compacted => saturating_add(&self.compacted, 1),
            CompactionOutcome::NoChange => saturating_add(&self.no_change, 1),
            CompactionOutcome::Failed => saturating_add(&self.failed, 1),
        }
        if cancelled {
            saturating_add(&self.cancelled, 1);
        }
        saturating_add(&self.elapsed_ms_total, elapsed_ms);
        let _ = self.elapsed_ms_max.fetch_max(elapsed_ms, Ordering::Relaxed);
    }

    fn snapshot(&self) -> CompactionMetrics {
        CompactionMetrics {
            started: self.started.load(Ordering::Relaxed),
            compacted: self.compacted.load(Ordering::Relaxed),
            no_change: self.no_change.load(Ordering::Relaxed),
            failed: self.failed.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            elapsed_ms_total: self.elapsed_ms_total.load(Ordering::Relaxed),
            elapsed_ms_max: self.elapsed_ms_max.load(Ordering::Relaxed),
        }
    }
}

/// Increment a counter, saturating instead of wrapping at `u64::MAX`.
fn saturating_add(counter: &AtomicU64, amount: u64) {
    // A saturated counter is a terminal state for the process lifetime, so the
    // previous value is intentionally discarded rather than reported.
    let _previous =
        counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| Some(current.saturating_add(amount)));
}

static COMPACTION_COUNTERS: CompactionCounters = CompactionCounters::new();

/// Return the cumulative automatic-compaction counters.
///
/// Reading the snapshot neither mutates nor resets the counters.
pub fn compaction_metrics() -> CompactionMetrics {
    COMPACTION_COUNTERS.snapshot()
}

/// Record that an automatic compaction attempt started.
pub(crate) fn record_compaction_started() {
    COMPACTION_COUNTERS.record_started();
}

/// Record the terminal outcome and duration of an automatic compaction attempt.
pub(crate) fn record_compaction_finished(outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64) {
    COMPACTION_COUNTERS.record_finished(outcome, cancelled, elapsed_ms);
}

/// Snapshot of the cumulative publication counters for one ACP send boundary.
///
/// `failures` is a subset of `attempts`: it counts the sends that returned an
/// error. A notice that was never built and a serialization failure that never
/// reached the transport are not attempts, so they move neither counter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublicationMetrics {
    /// Sends attempted at this boundary.
    pub attempts: u64,
    /// Attempts whose send returned an error.
    pub failures: u64,
}

/// Cumulative counters behind one publication boundary.
struct PublicationCounters {
    attempts: AtomicU64,
    failures: AtomicU64,
}

impl PublicationCounters {
    const fn new() -> Self {
        Self {
            attempts: AtomicU64::new(0),
            failures: AtomicU64::new(0),
        }
    }

    fn record(&self, sent: bool) {
        saturating_add(&self.attempts, 1);
        if !sent {
            saturating_add(&self.failures, 1);
        }
    }

    fn snapshot(&self) -> PublicationMetrics {
        PublicationMetrics {
            attempts: self.attempts.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

static NOTICE_COUNTERS: PublicationCounters = PublicationCounters::new();
static COMPACTION_UPDATE_COUNTERS: PublicationCounters = PublicationCounters::new();

/// Return the cumulative provider rate-limit notice publication counters.
pub fn notice_metrics() -> PublicationMetrics {
    NOTICE_COUNTERS.snapshot()
}

/// Return the cumulative compaction activity-update publication counters.
pub fn compaction_update_metrics() -> PublicationMetrics {
    COMPACTION_UPDATE_COUNTERS.snapshot()
}

/// Record one attempt to publish a provider rate-limit notice.
pub(crate) fn record_notice_publish(sent: bool) {
    NOTICE_COUNTERS.record(sent);
}

/// Record one attempt to publish a compaction activity update.
pub(crate) fn record_compaction_update_publish(sent: bool) {
    COMPACTION_UPDATE_COUNTERS.record(sent);
}

#[cfg(test)]
mod tests {
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

    /// Assert counter increments saturate rather than wrapping.
    #[test]
    fn counters_saturate_at_the_maximum() {
        let counter = AtomicU64::new(u64::MAX);
        saturating_add(&counter, 1);
        assert_eq!(counter.load(Ordering::Relaxed), u64::MAX);
    }
}
