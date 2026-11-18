//! Lifecycle ordering regressions for concurrently published snapshots.
use super::projection_tests::{subagent_event, with_updated_at};
use super::*;

/// The controller stamps the initial snapshot and the live transition from the
/// same clock, so an initial `Queued` snapshot captured before a live `Running`
/// transition can carry the same `updated_at` and arrive after it. Only the
/// initial-snapshot flag distinguishes the two, and the stale snapshot must not
/// be admitted.
#[test]
fn an_initial_snapshot_does_not_undo_live_progress_at_the_same_timestamp() {
    let mut ordering = LifecycleOrdering::default();
    let live_running = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"),
        initial_snapshot: false,
    };
    let queued_snapshot = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("queued"), "2026-08-14T12:01:00Z"),
        initial_snapshot: true,
    };

    assert!(ordering.admit(&live_running), "the live transition must be forwarded");
    assert!(
        !ordering.admit(&queued_snapshot),
        "an initial snapshot at the same instant must not undo live progress"
    );
}
