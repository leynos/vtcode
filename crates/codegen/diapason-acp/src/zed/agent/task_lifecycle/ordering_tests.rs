//! Lifecycle ordering regressions for concurrently published snapshots.
use super::projection_tests::{subagent_event, with_updated_at};
use super::*;

/// The initial snapshot pass and live observation race on the queue, and both
/// snapshots come from one controller clock. Two transitions recorded in one
/// tick therefore share `updated_at`, so the timestamp cannot order them. Both
/// paths are stamped from the same clock, the forwarder orders by revision, and
/// a snapshot that read an earlier state must not be admitted.
#[test]
fn an_initial_snapshot_does_not_undo_live_progress_at_the_same_timestamp() {
    let mut ordering = LifecycleOrdering::default();
    let live_running = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"),
        revision: 1,
    };
    let queued_snapshot = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("queued"), "2026-08-14T12:01:00Z"),
        revision: 0,
    };

    assert!(ordering.admit(&live_running), "the live transition must be forwarded");
    assert!(
        !ordering.admit(&queued_snapshot),
        "an initial snapshot with an older revision must not undo live progress"
    );
}

/// A capture that read an earlier state must lose to a snapshot read later.
///
/// This is the read-order rule the initial pass relies on: the pass reserves its
/// revision between the two reads that confirm a batch, so a capture that read
/// `Queued` before those reads is stamped below the snapshot's `Running`, while
/// a capture that read after them is stamped above it. Whichever order the two
/// are delivered in, the client ends on the later state.
#[test]
fn a_snapshot_read_after_a_capture_supersedes_that_capture() {
    let queued_capture = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("queued"), "2026-08-14T12:01:00Z"),
        revision: 4,
    };
    let running_snapshot = QueuedLifecycleUpdate {
        update: with_updated_at(subagent_event("running"), "2026-08-14T12:02:00Z"),
        revision: 5,
    };

    // Delivered in read order.
    let mut ordering = LifecycleOrdering::default();
    assert!(ordering.admit(&queued_capture));
    assert!(ordering.admit(&running_snapshot), "the later read must win");
    assert!(!ordering.admit(&queued_capture), "the delayed earlier capture must not roll the task back");

    // Delivered snapshot first, with the earlier capture arriving late.
    let mut reordered = LifecycleOrdering::default();
    assert!(reordered.admit(&running_snapshot));
    assert!(
        !reordered.admit(&queued_capture),
        "a capture stamped before the snapshot must not overwrite the newer state"
    );
}

/// Unwraps one delegated-child snapshot from its queued update.
fn subagent_entry(update: LifecycleUpdate) -> SubagentStatusEntry {
    match update {
        LifecycleUpdate::Subagent { task, .. } => task,
        other => panic!("expected a subagent snapshot, got {other:?}"),
    }
}

/// An older capture delivered after a newer one must still lose.
///
/// The controller stamps the revision while it captures the state, so the
/// reversed delivery order cannot change which revision each snapshot
/// carries. Stamping as the item reached the queue instead gave the delayed
/// older capture the higher revision, and the forwarder then admitted it over
/// the newer state it had already sent.
#[test]
fn a_delayed_older_capture_is_dropped_by_its_carried_revision() {
    let metrics = Arc::new(diapason_core::metrics::MetricsCollector::new());
    let queue_metrics = Arc::clone(&metrics) as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let (sender, mut receiver) = mpsc::channel(4);
    let observer = AcpTaskLifecycleObserver {
        session_id: "parent-session".to_string(),
        sender,
        queue: Arc::new(LifecycleQueueTracker::new(queue_metrics)),
    };
    let older = subagent_entry(with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"));
    let newer = subagent_entry(with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"));

    // The controller captured `older` first and hands each snapshot the
    // revision of its capture; deliver them in the opposite order.
    let older_revision = 4_u64;
    let newer_revision = 5_u64;
    observer.subagent_status_published(newer, newer_revision);
    observer.subagent_status_published(older, older_revision);

    let first = receiver.try_recv().expect("the newer capture must be queued");
    let second = receiver.try_recv().expect("the older capture must be queued");
    assert_eq!(first.revision, newer_revision);
    assert_eq!(second.revision, older_revision);
    assert!(
        first.revision > second.revision,
        "the delayed older capture must not take a newer revision than the state already sent"
    );

    let mut ordering = LifecycleOrdering::default();
    assert!(ordering.admit(&first), "the newer state must be forwarded");
    assert!(
        !ordering.admit(&second),
        "a capture taken before the newer state must not overwrite it when it arrives late"
    );
}
