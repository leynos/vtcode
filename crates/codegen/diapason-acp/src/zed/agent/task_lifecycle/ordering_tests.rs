//! Lifecycle ordering regressions for concurrently published snapshots.
use super::projection_tests::{subagent_event, with_updated_at};
use super::*;

/// The initial snapshot pass and live observation race on the queue, and both
/// snapshots come from one controller clock. Two transitions recorded in one
/// tick therefore share `updated_at`, so the timestamp cannot order them. The
/// adapter reserves one base revision for the snapshot pass and every live
/// transition takes a higher revision; the forwarder orders by revision, and a
/// stale snapshot must not be admitted.
#[test]
fn an_initial_snapshot_does_not_undo_live_progress_at_the_same_timestamp() {
    let mut ordering = LifecycleOrdering::default();
    let snapshot_revision = 0_u64;
    let live_running = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"),
        revision: 1,
    };
    let queued_snapshot = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("queued"), "2026-08-14T12:01:00Z"),
        revision: snapshot_revision,
    };

    assert!(ordering.admit(&live_running), "the live transition must be forwarded");
    assert!(
        !ordering.admit(&queued_snapshot),
        "an initial snapshot with the base revision must not undo live progress"
    );
}
