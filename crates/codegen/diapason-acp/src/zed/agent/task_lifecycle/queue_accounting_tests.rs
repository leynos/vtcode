//! Queue-accounting invariants for lifecycle notification publication.
use super::projection_tests::{background_event, subagent_event};
use super::*;
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
        .send(&sender, subagent_event("running"))
        .await
        .expect("an awaited initial snapshot must enqueue");
    queue
        .try_send(&sender, background_event("running"))
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
        .try_send(&sender, subagent_event("completed"))
        .expect("queued before close");
    assert_eq!(queue.depth(), 1);
    queue.close_and_drain();
    assert_eq!(metrics.drained.load(Ordering::SeqCst), 1, "the guard must account for the leftover item");
    assert_eq!(queue.depth(), 0);
    assert_eq!(
        queue
            .try_send(&sender, subagent_event("completed"))
            .expect_err("closed try_send"),
        LifecycleQueueDropKind::Closed
    );
    assert_eq!(
        queue
            .send(&sender, subagent_event("completed"))
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
        .send(&full_sender, subagent_event("running"))
        .await
        .expect("fills the only slot");
    assert_eq!(
        full_queue
            .try_send(&full_sender, subagent_event("completed"))
            .expect_err("full channel"),
        LifecycleQueueDropKind::Full
    );
    assert_eq!(full_queue.depth(), 1);
    assert_eq!(full_metrics.enqueued.load(Ordering::SeqCst), 1);
    drop(full_receiver);
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
        queue.send(&sender, subagent_event("running")).await.expect("awaited enqueue");
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
