//! Standard ACP lifecycle projection and transport regressions.
use super::super::lody::lody_task_session_update;
use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

fn subagent_event(status: &str) -> LifecycleUpdate {
    subagent_event_for("parent-session", status)
}

fn subagent_event_for(parent_session_id: &str, status: &str) -> LifecycleUpdate {
    LifecycleUpdate::Subagent {
        parent_session_id: parent_session_id.to_string(),
        task: serde_json::from_value(json!({
            "id": "child-1",
            "session_id": "child-session",
            "parent_thread_id": "parent-session",
            "agent_name": "worker",
            "display_label": "Worker",
            "description": "Implement one bounded task",
            "source": "builtin",
            "status": status,
            "background": false,
            "depth": 1,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:01:00Z",
            "summary": "Progress summary"
        }))
        .expect("valid subagent status fixture"),
    }
}

fn background_event(status: &str) -> LifecycleUpdate {
    LifecycleUpdate::BackgroundProcess {
        parent_session_id: "parent-session".to_string(),
        task: serde_json::from_value(json!({
            "id": "background-1",
            "session_id": "background-session",
            "exec_session_id": "exec-background-1",
            "agent_name": "watcher",
            "display_label": "Watcher",
            "description": "Watch the workspace",
            "source": "workspace",
            "status": status,
            "desired_enabled": true,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:01:00Z",
            "pid": 1234,
            "summary": "Watching"
        }))
        .expect("valid background status fixture"),
    }
}

fn task_meta(update: &acp::SessionUpdate) -> &Value {
    let meta = match update {
        acp::SessionUpdate::ToolCall(call) => call.meta.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(update) => update.meta.as_ref(),
        other => panic!("expected task-carrying tool update, got {other:?}"),
    }
    .expect("task update metadata");
    &meta["lody"]["task"]
}

fn with_updated_at(mut event: LifecycleUpdate, value: &str) -> LifecycleUpdate {
    let timestamp: DateTime<Utc> = value.parse().expect("RFC 3339 transition timestamp");
    match &mut event {
        LifecycleUpdate::Subagent { task, .. } => task.updated_at = timestamp,
        LifecycleUpdate::BackgroundProcess { task, .. } => task.updated_at = timestamp,
    }
    event
}

fn with_task_id(mut event: LifecycleUpdate, task_id: String) -> LifecycleUpdate {
    match &mut event {
        LifecycleUpdate::Subagent { task, .. } => task.id = task_id,
        LifecycleUpdate::BackgroundProcess { task, .. } => task.id = task_id,
    }
    event
}

#[test]
fn maps_worker_and_background_states_to_lody_task_metadata() {
    let cases = [
        (subagent_event("queued"), "subagent", "pending"),
        (subagent_event("running"), "subagent", "in_progress"),
        (subagent_event("waiting"), "subagent", "in_progress"),
        (subagent_event("completed"), "subagent", "completed"),
        (subagent_event("failed"), "subagent", "failed"),
        (subagent_event("closed"), "subagent", "failed"),
        (background_event("starting"), "background", "pending"),
        (background_event("running"), "background", "in_progress"),
        (background_event("stopped"), "background", "completed"),
        (background_event("error"), "background", "failed"),
    ];

    for (event, kind, status) in cases {
        let update = lody_task_session_update(event, false).expect("valid Lody task update");
        let acp::SessionUpdate::ToolCall(call) = &update else {
            panic!("first task snapshot must be a tool call");
        };
        let task_id = task_meta(&update)["taskId"].as_str().expect("task ID string");
        assert_eq!(call.tool_call_id.0.as_ref(), format!("task:{task_id}"));
        assert_eq!(call.kind, acp::ToolKind::Think);
        assert_eq!(task_meta(&update)["version"], 1);
        assert_eq!(task_meta(&update)["kind"], kind);
        assert_eq!(task_meta(&update)["status"], status);
        assert!(task_meta(&update).get("sessionId").is_none());
        assert!(task_meta(&update).get("pid").is_none());
        assert!(task_meta(&update).get("transcriptPath").is_none());
    }
}

fn lifecycle_ladder(kind_index: usize) -> (&'static [&'static str], &'static [u8]) {
    const SUBAGENT: &[&str] = &["queued", "running", "waiting", "completed", "failed", "closed"];
    const SUBAGENT_ORDINALS: &[u8] = &[0, 1, 1, 2, 2, 2];
    const BACKGROUND: &[&str] = &["starting", "running", "stopped", "error"];
    const BACKGROUND_ORDINALS: &[u8] = &[0, 1, 2, 2];
    if kind_index == 0 {
        (SUBAGENT, SUBAGENT_ORDINALS)
    } else {
        (BACKGROUND, BACKGROUND_ORDINALS)
    }
}

fn projected_ordinal(status: &str) -> u8 {
    match status {
        "pending" => 0,
        "in_progress" => 1,
        "completed" | "failed" => 2,
        other => panic!("unexpected projected task status {other}"),
    }
}

proptest! {
    /// A task's lifecycle walk is a non-decreasing sequence of states. Whatever
    /// its length, projection must keep one stable tool-call ID and must never
    /// report a terminal state as pending or in progress again.
    #[test]
    fn lifecycle_sequences_keep_stable_ids_and_monotonic_terminality(
        kind_index in 0usize..2,
        steps in prop::collection::vec(0usize..8, 1..=32),
    ) {
        let (ladder, ordinals) = lifecycle_ladder(kind_index);
        // The two fixtures use different ids; the kind selects which.
        let expected_task_id = if kind_index == 0 { "child-1" } else { "background-1" };
        let expected_call_id = format!("task:{expected_task_id}");
        let mut walked = steps.into_iter().map(|step| step % ladder.len()).collect::<Vec<_>>();
        walked.sort_unstable();

        let mut last_ordinal = 0_u8;
        for (index, status_index) in walked.iter().copied().enumerate() {
            let status = ladder[status_index];
            let event = if kind_index == 0 { subagent_event(status) } else { background_event(status) };
            let update = lody_task_session_update(event, index > 0).expect("valid lifecycle projection");
            let task_id = match &update {
                acp::SessionUpdate::ToolCall(call) => {
                    prop_assert_eq!(index, 0, "only the first snapshot may create the tool call");
                    prop_assert_eq!(call.tool_call_id.0.as_ref(), expected_call_id.as_str());
                    task_meta(&update)["taskId"].as_str().expect("task ID")
                }
                acp::SessionUpdate::ToolCallUpdate(call_update) => {
                    prop_assert!(index > 0, "later snapshots must update the existing tool call");
                    prop_assert_eq!(call_update.tool_call_id.0.as_ref(), expected_call_id.as_str());
                    task_meta(&update)["taskId"].as_str().expect("task ID")
                }
                other => {
                    prop_assert!(false, "unexpected lifecycle projection {other:?}");
                    return Ok(());
                }
            };
            prop_assert_eq!(task_id, expected_task_id);
            let ordinal = projected_ordinal(task_meta(&update)["status"].as_str().expect("projected status"));
            prop_assert_eq!(ordinal, ordinals[status_index], "status {} must map to its ladder rung", status);
            prop_assert!(ordinal >= last_ordinal, "terminality must never regress for status {}", status);
            last_ordinal = ordinal;
        }
    }

    #[test]
    fn task_updates_keep_stable_ids_across_statuses(
        task_id in "[A-Za-z0-9_-]{1,64}",
        status_index in 0_usize..10,
    ) {
        let statuses = [
            "queued", "running", "waiting", "completed", "failed", "closed",
            "starting", "running", "stopped", "error",
        ];
        let event = if status_index < 6 {
            subagent_event(statuses[status_index])
        } else {
            background_event(statuses[status_index])
        };
        let event = with_task_id(event, task_id.clone());

        let initial = lody_task_session_update(event.clone(), false).expect("valid initial task update");
        let subsequent = lody_task_session_update(event, true).expect("valid subsequent task update");

        let acp::SessionUpdate::ToolCall(initial) = initial else {
            prop_assert!(false, "initial snapshot must create a tool call");
            return Ok(());
        };
        let acp::SessionUpdate::ToolCallUpdate(subsequent) = subsequent else {
            prop_assert!(false, "subsequent snapshot must update the tool call");
            return Ok(());
        };
        let expected_id = format!("task:{task_id}");
        prop_assert_eq!(initial.tool_call_id.0.as_ref(), expected_id.as_str());
        prop_assert_eq!(subsequent.tool_call_id.0.as_ref(), expected_id.as_str());
        let initial_task_id = initial.meta.as_ref().expect("initial meta")["lody"]["task"]["taskId"]
            .as_str()
            .expect("initial task ID");
        let subsequent_task_id = subsequent.meta.as_ref().expect("subsequent meta")["lody"]["task"]["taskId"]
            .as_str()
            .expect("subsequent task ID");
        prop_assert_eq!(initial_task_id, task_id.as_str());
        prop_assert_eq!(subsequent_task_id, task_id.as_str());
    }
}

#[test]
fn stale_initial_snapshot_is_rejected_after_a_terminal_transition() {
    let mut ordering = LifecycleOrdering::default();
    let running = with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z");
    let completed = with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z");

    assert!(ordering.admit(&running), "the first snapshot must be forwarded");
    assert!(ordering.admit(&completed), "a newer terminal transition must be forwarded");
    assert!(
        !ordering.admit(&running),
        "an initial snapshot captured before the terminal transition must not roll the task back"
    );

    // A transition the controller really recorded after settlement still wins.
    let resumed = with_updated_at(subagent_event("running"), "2026-08-14T12:06:00Z");
    assert!(ordering.admit(&resumed), "newer lifecycle transitions must still be forwarded");
}

#[test]
fn equal_timestamp_revisions_never_regress_terminality() {
    let mut ordering = LifecycleOrdering::default();
    let completed = with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z");
    assert!(ordering.admit(&completed));

    let same_instant_running = with_updated_at(subagent_event("running"), "2026-08-14T12:05:00Z");
    assert!(
        !ordering.admit(&same_instant_running),
        "a coarsened clock must not let a terminal task regress to running"
    );

    let mut background = LifecycleOrdering::default();
    let running = with_updated_at(background_event("running"), "2026-08-14T12:01:00Z");
    let stopped = with_updated_at(background_event("stopped"), "2026-08-14T12:05:00Z");
    assert!(background.admit(&running));
    assert!(background.admit(&stopped));
    assert!(
        !background.admit(&with_updated_at(background_event("running"), "2026-08-14T12:01:00Z")),
        "a stale background snapshot must not revive a stopped task"
    );
}

#[tokio::test]
async fn stale_initial_task_snapshot_is_not_forwarded_after_settlement() {
    let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
    let (progress_tx, progress_rx) = mpsc::channel(8);
    let (received_tx, mut received_rx) = mpsc::unbounded_channel();

    let agent_connection = Agent.builder().name("vtcode-task-ordering-test").connect_with(
        agent_channel,
        move |cx: ConnectionTo<Client>| async move {
            let client = ConnectionHandle::new(cx);
            let metrics = Arc::new(vtcode_core::metrics::MetricsCollector::new());
            let queue = Arc::new(LifecycleQueueTracker::new(metrics));
            let forwarder = spawn_task_lifecycle_forwarder(
                progress_rx,
                acp::SessionId::new(Arc::from("parent-session")),
                client,
                Arc::clone(&queue),
            );
            // Live delivery first, then the older snapshot the initial
            // publisher captured before the terminal transition.
            queue
                .try_send(&progress_tx, with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"))
                .unwrap();
            queue
                .try_send(&progress_tx, with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"))
                .unwrap();
            queue
                .try_send(&progress_tx, with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"))
                .unwrap();
            drop(progress_tx);
            forwarder.await.expect("lifecycle forwarder");
            Ok(())
        },
    );
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(received_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |_cx: ConnectionTo<Agent>| {
            let first = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("initial task update deadline")
                .expect("initial task update");
            let acp::AgentNotification::SessionNotification(first) = first else {
                panic!("task lifecycle must use standard session notifications");
            };
            let acp::SessionUpdate::ToolCall(call) = first.update else {
                panic!("first task snapshot must create a tool call");
            };
            assert_eq!(call.meta.expect("initial task metadata")["lody"]["task"]["status"], "in_progress");

            let second = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("task progress update deadline")
                .expect("task progress update");
            let acp::AgentNotification::SessionNotification(second) = second else {
                panic!("task progress must remain a standard session notification");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = second.update else {
                panic!("later task snapshots must update the existing tool call");
            };
            assert_eq!(update.meta.expect("progress task metadata")["lody"]["task"]["status"], "completed");

            let stale = tokio::time::timeout(std::time::Duration::from_millis(250), received_rx.recv()).await;
            assert!(stale.is_err(), "a stale initial snapshot must not be forwarded after settlement");
            Ok(())
        });

    tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
        .await
        .expect("client connection deadline")
        .expect("client connection");
    agent_task.await.expect("agent task").expect("agent connection");
}

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

#[tokio::test]
async fn forwards_lifecycle_events_through_the_official_acp_duplex() {
    let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
    let (progress_tx, progress_rx) = mpsc::channel(8);
    let (received_tx, mut received_rx) = mpsc::unbounded_channel();

    let agent_connection = Agent.builder().name("vtcode-task-lifecycle-test").connect_with(
        agent_channel,
        move |cx: ConnectionTo<Client>| async move {
            let client = ConnectionHandle::new(cx);
            let metrics = Arc::new(vtcode_core::metrics::MetricsCollector::new());
            let queue = Arc::new(LifecycleQueueTracker::new(metrics));
            let forwarder = spawn_task_lifecycle_forwarder(
                progress_rx,
                acp::SessionId::new(Arc::from("parent-session")),
                client,
                Arc::clone(&queue),
            );
            queue
                .try_send(&progress_tx, subagent_event_for("other-session", "running"))
                .unwrap();
            queue.try_send(&progress_tx, subagent_event("running")).unwrap();
            queue.try_send(&progress_tx, subagent_event("completed")).unwrap();
            drop(progress_tx);
            forwarder.await.expect("lifecycle forwarder");
            Ok(())
        },
    );
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(received_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |_cx: ConnectionTo<Agent>| {
            let first = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("initial task update deadline")
                .expect("initial task update");
            let acp::AgentNotification::SessionNotification(first) = first else {
                panic!("task lifecycle must use standard session notifications");
            };
            assert_eq!(first.session_id.0.as_ref(), "parent-session");
            let acp::SessionUpdate::ToolCall(call) = first.update else {
                panic!("first task snapshot must create a tool call");
            };
            assert_eq!(call.tool_call_id.0.as_ref(), "task:child-1");
            assert_eq!(call.meta.expect("initial task metadata")["lody"]["task"]["status"], "in_progress");

            let second = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("task progress update deadline")
                .expect("task progress update");
            let acp::AgentNotification::SessionNotification(second) = second else {
                panic!("task progress must remain a standard session notification");
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = second.update else {
                panic!("later task snapshots must update the existing tool call");
            };
            assert_eq!(update.tool_call_id.0.as_ref(), "task:child-1");
            assert_eq!(update.meta.expect("progress task metadata")["lody"]["task"]["status"], "completed");
            assert!(received_rx.try_recv().is_err(), "foreign-session events must remain filtered");
            Ok(())
        });

    tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
        .await
        .expect("client connection deadline")
        .expect("client connection");
    agent_task.await.expect("agent task").expect("agent connection");
}
