//! Sequenced-input properties for the counter arithmetic.
//!
//! The fixed examples in the parent module document each rule once. These
//! properties drive the same recorders through arbitrary event sequences
//! and compare every snapshot with a reference model that shares no
//! arithmetic with the implementation, so an accumulation or clamping
//! mistake shows up for some sequence rather than only for a chosen one.

use super::*;
use proptest::prelude::*;

/// One event at the compaction command boundary.
///
/// Starts and finishes are generated independently rather than paired, so
/// a sequence can also stop an attempt that never started, start one that
/// never finishes, or interleave several.
#[derive(Clone, Copy, Debug)]
enum Event {
    Started,
    Finished {
        outcome: CompactionOutcome,
        cancelled: bool,
        elapsed_ms: u64,
    },
}

fn event_strategy() -> impl Strategy<Value = Event> {
    prop_oneof![
        Just(Event::Started),
        (0_u8..3, any::<bool>(), any::<u64>()).prop_map(|(outcome, cancelled, elapsed_ms)| {
            let outcome = match outcome {
                0 => CompactionOutcome::Failed,
                1 => CompactionOutcome::Compacted,
                _ => CompactionOutcome::NoChange,
            };
            // A cancelled flag only accompanies a failure; any other
            // pairing is not reachable at the command boundary.
            Event::Finished {
                outcome,
                cancelled: cancelled && outcome == CompactionOutcome::Failed,
                elapsed_ms,
            }
        }),
    ]
}

/// Reference model for the compaction counters.
#[derive(Debug, Default)]
struct CompactionModel {
    started: u64,
    compacted: u64,
    no_change: u64,
    failed: u64,
    cancelled: u64,
    elapsed_total: u128,
    elapsed_max: u64,
}

impl CompactionModel {
    fn record(&mut self, event: Event) {
        match event {
            Event::Started => self.started += 1,
            Event::Finished { outcome, cancelled, elapsed_ms } => {
                match outcome {
                    CompactionOutcome::Compacted => self.compacted += 1,
                    CompactionOutcome::NoChange => self.no_change += 1,
                    CompactionOutcome::Failed => self.failed += 1,
                }
                if cancelled {
                    self.cancelled += 1;
                }
                self.elapsed_total += u128::from(elapsed_ms);
                self.elapsed_max = self.elapsed_max.max(elapsed_ms);
            }
        }
    }

    /// Every finished attempt lands in exactly one outcome bucket.
    fn finished(&self) -> u64 {
        self.compacted + self.no_change + self.failed
    }
}

proptest! {
    /// Assert the published compaction snapshot equals an independent model.
    ///
    /// The recorder has one writer here, so each snapshot is taken after a
    /// quiescent sequence: the fields were all updated before it was read,
    /// and the finished-count identity is expected to hold exactly. The
    /// concurrent-ordering clamps are covered separately by the fixed
    /// examples in the parent module.
    #[test]
    fn compaction_counters_match_an_independent_model(events in prop::collection::vec(event_strategy(), 0..32)) {
        let counters = CompactionCounters::new();
        let mut model = CompactionModel::default();

        for event in events {
            match event {
                Event::Started => counters.record_started(),
                Event::Finished { outcome, cancelled, elapsed_ms } => {
                    counters.record_finished(outcome, cancelled, elapsed_ms);
                }
            }
            model.record(event);
        }

        let snapshot = counters.snapshot();
        prop_assert_eq!(snapshot.started, model.started);
        prop_assert_eq!(snapshot.compacted, model.compacted);
        prop_assert_eq!(snapshot.no_change, model.no_change);
        prop_assert_eq!(snapshot.cancelled, model.cancelled);
        prop_assert_eq!(snapshot.failed, model.failed.max(model.cancelled));
        prop_assert_eq!(
            u128::from(snapshot.elapsed_ms_total),
            model.elapsed_total.min(u128::from(u64::MAX)),
            "the published total saturates at the ceiling rather than wrapping"
        );
        prop_assert_eq!(snapshot.elapsed_ms_max, model.elapsed_max);

        // The documented invariants, checked against the model rather than
        // against the snapshot alone.
        prop_assert!(snapshot.failed >= snapshot.cancelled, "cancelled is a subset of failed");
        prop_assert_eq!(snapshot.compacted + snapshot.no_change + snapshot.failed, model.finished());
    }

    /// Assert publication counters match an independent model.
    #[test]
    fn publication_counters_match_an_independent_model(sends in prop::collection::vec(any::<bool>(), 0..64)) {
        let counters = PublicationCounters::new();
        let mut attempts = 0_u64;
        let mut failures = 0_u64;

        for sent in sends {
            counters.record(sent);
            attempts += 1;
            if !sent {
                failures += 1;
            }
        }

        let snapshot = counters.snapshot();
        prop_assert_eq!(snapshot.attempts, attempts);
        prop_assert_eq!(snapshot.failures, failures);
        prop_assert!(snapshot.attempts >= snapshot.failures, "failures are a subset of attempts");
    }
}
