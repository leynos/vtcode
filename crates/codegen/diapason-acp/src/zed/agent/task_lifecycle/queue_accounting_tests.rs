//! Queue-accounting invariants for lifecycle notification publication.
use super::projection_tests::{background_event, live, live_at, subagent_event, with_task_id, with_updated_at};
use super::queue_accounting::LifecycleQueueDropKind;
use super::*;
use diapason_core::metrics::TaskLifecycleNotificationQueueDrop;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

/// Counts each accounting callback so the test can assert that every enqueue
/// path reaches the same driven port, not merely the tracker's own counter.
#[derive(Default)]
struct RecordingQueueMetrics {
    enqueued: AtomicUsize,
    dequeued: AtomicUsize,
    drained: AtomicUsize,
}

impl TaskLifecycleNotificationQueueMetricsObserver for RecordingQueueMetrics {
    fn queue_item_enqueued(&self) {
        let _ = self.enqueued.fetch_add(1, Ordering::SeqCst);
    }

    fn queue_item_dequeued(&self) {
        let _ = self.dequeued.fetch_add(1, Ordering::SeqCst);
    }

    fn queue_items_drained(&self, entries: usize) {
        let _ = self.drained.fetch_add(entries, Ordering::SeqCst);
    }

    fn queue_item_dropped(&self, _reason: TaskLifecycleNotificationQueueDrop) -> u64 {
        0
    }
}

/// The attach-time publisher awaits capacity while live observers use
/// `try_send`. If only one path incremented depth, a single dequeue would
/// cancel two pending updates and report an empty queue that is not empty.
#[tokio::test]
async fn initial_snapshot_enqueue_shares_accounting_with_live_updates() {
    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = LifecycleQueueTracker::new(observer);
    let (sender, mut receiver) = mpsc::channel(4);

    queue
        .send(&sender, live(subagent_event("running")))
        .await
        .expect("an awaited initial snapshot must enqueue");
    queue
        .try_send(&sender, live(background_event("running")))
        .expect("a live update must enqueue");
    assert_eq!(queue.depth(), 2, "both enqueue paths must count against one depth");
    assert_eq!(metrics.enqueued.load(Ordering::SeqCst), 2);

    // The forwarder decrements once per item it takes off the channel.
    drop(receiver.recv().await.expect("first queued update"));
    queue.dequeued();
    assert_eq!(queue.depth(), 1, "one dequeue must not cancel two enqueues");
    drop(receiver.recv().await.expect("second queued update"));
    queue.dequeued();
    assert_eq!(queue.depth(), 0);
    assert_eq!(metrics.dequeued.load(Ordering::SeqCst), 2);

    // Closing drains what is still pending and rejects both paths afterwards.
    queue
        .try_send(&sender, live(subagent_event("completed")))
        .expect("queued before close");
    assert_eq!(queue.depth(), 1);
    queue.close_and_drain();
    assert_eq!(metrics.drained.load(Ordering::SeqCst), 1, "the guard must account for the leftover item");
    assert_eq!(queue.depth(), 0);
    assert_eq!(
        queue
            .try_send(&sender, live(subagent_event("completed")))
            .expect_err("closed try_send"),
        LifecycleQueueDropKind::Closed
    );
    assert_eq!(
        queue
            .send(&sender, live(subagent_event("completed")))
            .await
            .expect_err("closed awaited send"),
        LifecycleQueueDropKind::Closed
    );
    assert_eq!(queue.depth(), 0, "a refused enqueue must not inflate depth");
    assert_eq!(metrics.enqueued.load(Ordering::SeqCst), 3, "only accepted enqueues may be counted");
    drop(receiver);

    // Bounded delivery is preserved: a full channel refuses instead of
    // waiting, and a refused item is not counted.
    let full_metrics = Arc::new(RecordingQueueMetrics::default());
    let full_observer = full_metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let full_queue = LifecycleQueueTracker::new(full_observer);
    let (full_sender, full_receiver) = mpsc::channel(1);
    full_queue
        .send(&full_sender, live(subagent_event("running")))
        .await
        .expect("fills the only slot");
    assert_eq!(
        full_queue
            .try_send(&full_sender, live(subagent_event("completed")))
            .expect_err("full channel"),
        LifecycleQueueDropKind::Full
    );
    assert_eq!(full_queue.depth(), 1);
    assert_eq!(full_metrics.enqueued.load(Ordering::SeqCst), 1);
    drop(full_receiver);
}

/// A live update the full queue refuses is recovered, not lost.
///
/// The attach-time snapshot pass can hold every slot while a task completes.
/// The refusal must not leave the client showing that task as active: the
/// tracker keeps the newest refused update, and the forwarder's next read
/// hands it out once the queued snapshots have drained.
#[tokio::test]
async fn a_refused_update_is_held_until_the_forwarder_drains_the_channel() {
    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = LifecycleQueueTracker::new(observer);
    // One slot, so the snapshot still queued when the live update arrives is
    // what makes the refusal reproducible.
    let (sender, mut receiver) = mpsc::channel(1);
    let snapshot = live(subagent_event("running"));
    let snapshot_revision = snapshot.revision;
    queue
        .send(&sender, snapshot)
        .await
        .expect("the initial snapshot fills the only slot");

    let terminal = live(subagent_event("completed"));
    let terminal_revision = terminal.revision;
    assert_eq!(
        queue
            .try_send(&sender, terminal)
            .expect_err("the full queue refuses the live terminal update"),
        LifecycleQueueDropKind::Full
    );
    assert_eq!(queue.depth(), 1, "a parked update was never queued, so it is not counted");

    let mut ordering = LifecycleOrdering::default();
    let recovered = queue
        .next_update(&mut receiver)
        .await
        .expect("the refused terminal update must still be deliverable");
    assert_eq!(
        recovered.revision, terminal_revision,
        "the forwarder must take the parked terminal update rather than wait again on the channel"
    );
    assert!(ordering.admit(&recovered), "the terminal state is the newest revision, so it is forwarded");
    assert_eq!(queue.depth(), 1, "taking a parked update must not dequeue a channel item");

    let older = queue.next_update(&mut receiver).await.expect("the queued initial snapshot");
    assert_eq!(older.revision, snapshot_revision);
    assert!(!ordering.admit(&older), "the active snapshot the terminal state superseded must not overwrite it");
    assert_eq!(queue.depth(), 0);
}

/// One task's refusal must not evict another task's parked terminal state.
///
/// Two tasks finish while the queue is full. A single parked slot would keep
/// only the later refusal: the forwarder compares revisions per task, so task
/// B's completion cannot stand in for task A's, and the client would receive A's
/// queued active snapshot with no terminal update left to correct it. Parking
/// per task recovers both, and a repeated refusal of the same task still keeps
/// only its newest revision.
#[tokio::test]
async fn two_task_saturation_recovers_both_terminal_states() {
    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = LifecycleQueueTracker::new(observer);
    // Two slots, so both tasks' active snapshots can be in flight when the
    // queue refuses their completions.
    let (sender, mut receiver) = mpsc::channel(2);
    let task_a = || with_task_id(subagent_event("running"), "task-a".to_string());
    let task_b = || with_task_id(subagent_event("running"), "task-b".to_string());
    let snapshot_revision = 7_u64;
    queue
        .send(&sender, live_at(snapshot_revision, task_a()))
        .await
        .expect("task A's active snapshot fills the first slot");
    queue
        .send(&sender, live_at(snapshot_revision, task_b()))
        .await
        .expect("task B's active snapshot fills the second slot");

    let a_completed = live_at(
        8,
        with_task_id(with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"), "task-a".to_string()),
    );
    let b_completed = live_at(
        9,
        with_task_id(with_updated_at(subagent_event("completed"), "2026-08-14T12:06:00Z"), "task-b".to_string()),
    );
    for completion in [&a_completed, &b_completed] {
        assert_eq!(
            queue
                .try_send(
                    &sender,
                    QueuedLifecycleUpdate {
                        update: completion.update.clone(),
                        revision: completion.revision
                    }
                )
                .expect_err("the full queue refuses each completion"),
            LifecycleQueueDropKind::Full
        );
    }
    assert_eq!(queue.depth(), 2, "refused updates were never queued, so they are not counted");

    let mut ordering = LifecycleOrdering::default();
    let mut delivered = Vec::new();
    for _ in 0..4 {
        let queued = queue
            .next_update(&mut receiver)
            .await
            .expect("every queued and parked update is deliverable");
        if ordering.admit(&queued) {
            delivered.push(queued);
        }
    }

    let completed_tasks = delivered
        .iter()
        .filter(|queued| queued.update.is_terminal())
        .map(|queued| queued.task_id().to_owned())
        .collect::<Vec<_>>();
    assert!(
        completed_tasks.contains(&"task-a".to_string()) && completed_tasks.contains(&"task-b".to_string()),
        "both terminal states must survive the saturation, got {completed_tasks:?}"
    );
    assert_eq!(delivered.len(), 2, "each task's terminal state supersedes its own queued active snapshot");
    assert_eq!(queue.depth(), 0, "the channel drained completely");
}

/// Repeated refusals of one task keep only that task's newest revision.
///
/// Per-task parking must not turn into one slot per revision: a task that
/// changes twice while the queue is full has one state worth delivering, and a
/// later revision supersedes the earlier one exactly as it would on the wire.
#[tokio::test]
async fn repeated_refusals_of_one_task_keep_only_its_newest_revision() {
    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = LifecycleQueueTracker::new(observer);
    let (sender, mut receiver) = mpsc::channel(1);
    let active = with_task_id(subagent_event("running"), "task-a".to_string());
    queue
        .send(&sender, live_at(3, active))
        .await
        .expect("the active snapshot fills the only slot");

    let completed = live_at(
        4,
        with_task_id(with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"), "task-a".to_string()),
    );
    let failed = live_at(
        5,
        with_task_id(with_updated_at(subagent_event("failed"), "2026-08-14T12:06:00Z"), "task-a".to_string()),
    );
    for refusal in [&completed, &failed] {
        assert_eq!(
            queue
                .try_send(
                    &sender,
                    QueuedLifecycleUpdate {
                        update: refusal.update.clone(),
                        revision: refusal.revision
                    }
                )
                .expect_err("the full queue refuses both transitions"),
            LifecycleQueueDropKind::Full
        );
    }

    let parked = queue
        .next_update(&mut receiver)
        .await
        .expect("the newest refusal is still deliverable");
    assert_eq!(parked.revision, 5, "the later revision must replace the earlier refusal of the same task");
    assert!(parked.update.is_terminal());
    let queued_active = queue
        .next_update(&mut receiver)
        .await
        .expect("the active snapshot is still queued");
    assert_eq!(queued_active.revision, 3);
    assert_eq!(queue.depth(), 0);
}

/// The forwarder decrements depth the moment it receives an item, so the count
/// must already be visible by then.
///
/// This is a concurrent invariant regression, not a deterministic
/// reproduction of the removed window: it starts the enqueue and the receiver
/// concurrently and checks the invariant at the receiver's observation point
/// for 256 rounds. The deterministic ordering proof lives beside it in
/// `the_pre_repair_publish_then_count_order_fails_the_invariant`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_forwarder_never_observes_an_unaccounted_item() {
    const ROUNDS: usize = 256;

    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = Arc::new(LifecycleQueueTracker::new(observer));
    let (sender, mut receiver) = mpsc::channel(1);

    let (observed_tx, mut observed_rx) = mpsc::unbounded_channel();
    let (release_tx, mut release_rx) = mpsc::unbounded_channel::<()>();
    let (dequeued_tx, mut dequeued_rx) = mpsc::unbounded_channel::<()>();
    let receiver_queue = Arc::clone(&queue);
    let receiver_task = tokio::spawn(async move {
        while let Some(update) = receiver.recv().await {
            observed_tx.send(receiver_queue.depth()).expect("report the observed depth");
            release_rx.recv().await.expect("release the receiver");
            drop(update);
            receiver_queue.dequeued();
            dequeued_tx.send(()).expect("report the dequeue");
        }
    });

    for round in 0..ROUNDS {
        let running = live(subagent_event("running"));
        queue.send(&sender, running).await.expect("awaited enqueue");
        let observed = observed_rx.recv().await.expect("the receiver observed the item");
        assert_eq!(observed, 1, "round {round}: an item must be counted before it is observable");
        release_tx.send(()).expect("release the receiver");
        dequeued_rx.recv().await.expect("the dequeue landed");
        assert_eq!(queue.depth(), 0, "round {round}: an empty channel must report zero depth");
    }

    drop(sender);
    receiver_task.await.expect("receiver task");
    assert_eq!(metrics.enqueued.load(Ordering::SeqCst), ROUNDS);
    assert_eq!(metrics.dequeued.load(Ordering::SeqCst), ROUNDS);
    assert_eq!(metrics.drained.load(Ordering::SeqCst), 0, "a balanced channel drains nothing");
}

/// Negative control: the pre-repair publish-then-count order fails the same
/// invariant deterministically, with no scheduler involvement.
///
/// The repaired tracker counts under the lock that publishes, so the item is
/// never observable while its depth is still zero. This control reproduces the
/// old order literally against the same tracker: it publishes through the
/// channel first, lets the forwarder receive and dequeue, and only then
/// records the enqueue. The observation point that must report one therefore
/// reports zero, the dequeue is lost at zero depth, and the late count leaves
/// a permanent phantom depth with the channel already empty. The production
/// paths cannot reach this state because they keep the state lock from before
/// publication until after the count.
#[test]
fn the_pre_repair_publish_then_count_order_fails_the_invariant() {
    let metrics = Arc::new(RecordingQueueMetrics::default());
    let observer = metrics.clone() as Arc<dyn TaskLifecycleNotificationQueueMetricsObserver>;
    let queue = LifecycleQueueTracker::new(observer);
    let (sender, mut receiver) = mpsc::channel(1);

    sender.try_send(subagent_event("running")).expect("the channel has room");
    assert_eq!(queue.depth(), 0, "publishing before counting exposes an item the tracked depth has not counted");

    drop(receiver.try_recv().expect("the forwarder received the published item"));
    queue.dequeued();
    {
        let mut state = queue.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        queue.record_enqueued_locked(&mut state);
    }

    assert_eq!(queue.depth(), 1, "the late count survives as a phantom depth");
    assert!(receiver.try_recv().is_err(), "the channel is empty although depth reports an item");
    assert_eq!(metrics.enqueued.load(Ordering::SeqCst), 1);
    assert_eq!(metrics.dequeued.load(Ordering::SeqCst), 0, "the dequeue was lost at zero depth");
}
