//! Bounded, process-local metrics for the ACP bridge.
//!
//! Counters are cumulative and never reset. Their cardinality is fixed by
//! construction: compaction outcomes come from [`CompactionOutcome`],
//! publication outcomes are the two states of a send, and no session, provider,
//! model, path, or payload value is ever recorded as a label.

use std::sync::Arc;
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
/// slightly different instants; the snapshot loads `cancelled` first and
/// clamps `failed` to at least it, so the subset invariant holds in every
/// snapshot.
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

    /// Snapshot the cumulative counters.
    ///
    /// The atomics are updated in separate steps, so the snapshot can reflect
    /// slightly different instants. `cancelled` is loaded first and `failed` is
    /// clamped to at least `cancelled`, so the published invariant — every
    /// cancelled attempt is one of the failures — holds in every snapshot.
    fn snapshot(&self) -> CompactionMetrics {
        let cancelled = self.cancelled.load(Ordering::Relaxed);
        let failed = self.failed.load(Ordering::Relaxed).max(cancelled);
        CompactionMetrics {
            started: self.started.load(Ordering::Relaxed),
            compacted: self.compacted.load(Ordering::Relaxed),
            no_change: self.no_change.load(Ordering::Relaxed),
            failed,
            cancelled,
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

/// A narrow sink for the outcomes of one compaction command boundary.
///
/// Compaction code depends on this port rather than on the process-wide
/// statics, so a command test can hold an isolated recorder and assert exact
/// deltas without racing another attempt that compacts in the same process.
pub(crate) trait CompactionRecorder: Send + Sync {
    /// Record that an attempt started.
    fn record_started(&self);

    /// Record the terminal outcome and duration of an attempt.
    fn record_finished(&self, outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64);

    /// Return this recorder's cumulative snapshot.
    fn snapshot(&self) -> CompactionMetrics;
}

impl CompactionRecorder for CompactionCounters {
    fn record_started(&self) {
        CompactionCounters::record_started(self);
    }

    fn record_finished(&self, outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64) {
        CompactionCounters::record_finished(self, outcome, cancelled, elapsed_ms);
    }

    fn snapshot(&self) -> CompactionMetrics {
        CompactionCounters::snapshot(self)
    }
}

/// Process-wide recorder for automatic compaction outcomes.
///
/// This is the edge implementation: production wiring installs it once, and a
/// test that needs exact deltas substitutes an isolated recorder instead.
struct ProcessCompactionRecorder(&'static CompactionCounters);

impl CompactionRecorder for ProcessCompactionRecorder {
    fn record_started(&self) {
        self.0.record_started();
    }

    fn record_finished(&self, outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64) {
        self.0.record_finished(outcome, cancelled, elapsed_ms);
    }

    fn snapshot(&self) -> CompactionMetrics {
        self.0.snapshot()
    }
}

/// An isolated compaction recorder owned by one test.
#[cfg(test)]
pub(crate) struct IsolatedCompactionRecorder {
    counters: CompactionCounters,
}

#[cfg(test)]
impl IsolatedCompactionRecorder {
    /// Create a recorder with no prior counts.
    pub(crate) fn new() -> Self {
        Self { counters: CompactionCounters::new() }
    }

    /// Return the counts recorded so far.
    pub(crate) fn metrics(&self) -> CompactionMetrics {
        self.counters.snapshot()
    }
}

#[cfg(test)]
impl CompactionRecorder for IsolatedCompactionRecorder {
    fn record_started(&self) {
        self.counters.record_started();
    }

    fn record_finished(&self, outcome: CompactionOutcome, cancelled: bool, elapsed_ms: u64) {
        self.counters.record_finished(outcome, cancelled, elapsed_ms);
    }

    fn snapshot(&self) -> CompactionMetrics {
        self.counters.snapshot()
    }
}

static COMPACTION_COUNTERS: CompactionCounters = CompactionCounters::new();

/// Return the cumulative automatic-compaction counters.
///
/// Reading the snapshot neither mutates nor resets the counters.
pub fn compaction_metrics() -> CompactionMetrics {
    COMPACTION_COUNTERS.snapshot()
}

/// Return the process-wide recorder for automatic compaction outcomes.
pub(crate) fn compaction_recorder() -> Arc<dyn CompactionRecorder> {
    Arc::new(ProcessCompactionRecorder(&COMPACTION_COUNTERS))
}

/// Snapshot of the cumulative publication counters for one ACP send boundary.
///
/// `failures` is a subset of `attempts`: it counts the sends that returned an
/// error. A notice that was never built and a serialization failure that never
/// reached the transport are not attempts, so they move neither counter. The
/// two fields are loaded independently, so a snapshot can reflect slightly
/// different instants; `attempts` is clamped to at least `failures` so the
/// subset invariant holds in every snapshot.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PublicationMetrics {
    /// Sends attempted at this boundary.
    pub attempts: u64,
    /// Attempts whose send returned an error.
    pub failures: u64,
}

/// A narrow sink for the send outcomes of one ACP publication boundary.
///
/// Publication code depends on this port rather than on the process-wide
/// statics, so a command test can hold an isolated recorder and assert exact
/// deltas without racing another test that publishes to the same boundary.
pub(crate) trait PublicationRecorder: Send + Sync {
    /// Record one send attempt and whether the transport accepted it.
    fn record(&self, sent: bool);

    /// Return this recorder's cumulative snapshot.
    fn snapshot(&self) -> PublicationMetrics;
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

    /// Snapshot the cumulative counters.
    ///
    /// The two atomics are updated in separate steps, so the pair can reflect
    /// slightly different instants. `attempts` is loaded after `failures` and
    /// clamped to at least `failures`, so the published invariant — every
    /// failure is one of the attempts — holds in every snapshot.
    fn snapshot(&self) -> PublicationMetrics {
        let failures = self.failures.load(Ordering::Relaxed);
        let attempts = self.attempts.load(Ordering::Relaxed);
        PublicationMetrics { attempts: attempts.max(failures), failures }
    }
}

impl PublicationRecorder for PublicationCounters {
    fn record(&self, sent: bool) {
        PublicationCounters::record(self, sent);
    }

    fn snapshot(&self) -> PublicationMetrics {
        PublicationCounters::snapshot(self)
    }
}

/// Process-wide recorder for one publication boundary.
///
/// This is the edge implementation: production wiring installs it once, and a
/// test that needs exact deltas substitutes an isolated recorder instead.
struct ProcessPublicationRecorder(&'static PublicationCounters);

impl PublicationRecorder for ProcessPublicationRecorder {
    fn record(&self, sent: bool) {
        self.0.record(sent);
    }

    fn snapshot(&self) -> PublicationMetrics {
        self.0.snapshot()
    }
}

/// An isolated publication recorder owned by one test.
#[cfg(test)]
pub(crate) struct IsolatedPublicationRecorder {
    counters: PublicationCounters,
}

#[cfg(test)]
impl IsolatedPublicationRecorder {
    /// Create a recorder with no prior counts.
    pub(crate) fn new() -> Self {
        Self { counters: PublicationCounters::new() }
    }

    /// Return the counts recorded so far.
    pub(crate) fn metrics(&self) -> PublicationMetrics {
        self.counters.snapshot()
    }
}

#[cfg(test)]
impl PublicationRecorder for IsolatedPublicationRecorder {
    fn record(&self, sent: bool) {
        self.counters.record(sent);
    }

    fn snapshot(&self) -> PublicationMetrics {
        self.counters.snapshot()
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

/// Return the process-wide recorder for provider rate-limit notices.
pub(crate) fn notice_recorder() -> Arc<dyn PublicationRecorder> {
    Arc::new(ProcessPublicationRecorder(&NOTICE_COUNTERS))
}

/// Return the process-wide recorder for compaction activity updates.
pub(crate) fn compaction_update_recorder() -> Arc<dyn PublicationRecorder> {
    Arc::new(ProcessPublicationRecorder(&COMPACTION_UPDATE_COUNTERS))
}

#[cfg(test)]
mod tests {
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
}
