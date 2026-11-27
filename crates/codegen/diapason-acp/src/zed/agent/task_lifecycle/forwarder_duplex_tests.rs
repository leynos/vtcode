//! Duplex transport regressions for the lifecycle forwarder.
use super::projection_tests::{live, subagent_event, subagent_event_for, with_task_id, with_updated_at};
use super::queue_accounting::LifecycleQueueDropKind;
use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use std::time::Duration;
use tokio::sync::mpsc;

/// Duplex connection with one lifecycle forwarder attached to the agent side.
struct ForwarderDuplex {
    received: mpsc::UnboundedReceiver<acp::AgentNotification>,
    agent_task: JoinHandle<agent_client_protocol::Result<()>>,
    client_task: JoinHandle<agent_client_protocol::Result<()>>,
}

impl ForwarderDuplex {
    /// Starts an agent connection whose forwarder publishes whatever `publish` sends.
    ///
    /// The client side only records notifications; each test asserts on the
    /// recorded stream and then calls [`Self::finish`].
    fn start(
        agent_name: &'static str,
        publish: impl FnOnce(mpsc::Sender<QueuedLifecycleUpdate>, Arc<LifecycleQueueTracker>) + Send + 'static,
    ) -> Self {
        let (progress_tx, progress_rx) = mpsc::channel(8);
        let metrics = Arc::new(diapason_core::metrics::MetricsCollector::new());
        let queue = Arc::new(LifecycleQueueTracker::new(metrics));
        publish(progress_tx, Arc::clone(&queue));
        Self::with_queue(agent_name, progress_rx, queue)
    }

    /// Starts a forwarder over a queue and channel the caller already prepared.
    ///
    /// The caller keeps ownership so it can fill the bounded channel and record
    /// a refusal against the tracker before the forwarder starts draining.
    fn with_queue(
        agent_name: &'static str,
        progress_rx: mpsc::Receiver<QueuedLifecycleUpdate>,
        queue: Arc<LifecycleQueueTracker>,
    ) -> Self {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let (received_tx, received) = mpsc::unbounded_channel();

        let agent_connection =
            Agent
                .builder()
                .name(agent_name)
                .connect_with(agent_channel, move |cx: ConnectionTo<Client>| async move {
                    let client = ConnectionHandle::new(cx);
                    let forwarder = spawn_task_lifecycle_forwarder(
                        progress_rx,
                        acp::SessionId::new(Arc::from("parent-session")),
                        client,
                        queue,
                    );
                    forwarder.await.expect("lifecycle forwarder");
                    Ok(())
                });
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
                std::future::pending::<agent_client_protocol::Result<()>>().await
            });
        let client_task = tokio::spawn(client_connection);

        Self { received, agent_task, client_task }
    }

    /// Waits for the next forwarded notification, returning `None` on timeout.
    async fn next(&mut self, deadline: Duration) -> Option<acp::AgentNotification> {
        tokio::time::timeout(deadline, self.received.recv()).await.ok().flatten()
    }

    /// Aborts the parked client and waits for the agent side to complete.
    async fn finish(self) {
        self.client_task.abort();
        drop(self.client_task.await);
        tokio::time::timeout(Duration::from_secs(3), self.agent_task)
            .await
            .expect("agent connection deadline")
            .expect("agent task")
            .expect("agent connection");
    }
}

#[tokio::test]
async fn stale_initial_task_snapshot_is_not_forwarded_after_settlement() {
    let mut duplex = ForwarderDuplex::start("diapason-task-ordering-test", |progress_tx, queue| {
        // Live delivery first, then a replay of the earlier transition under
        // the revision it was queued with. It arrives after settlement but
        // must not overwrite it.
        let running = live(with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"));
        let stale_revision = running.revision;
        queue.try_send(&progress_tx, running).unwrap();
        queue
            .try_send(&progress_tx, live(with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z")))
            .unwrap();
        queue
            .try_send(
                &progress_tx,
                QueuedLifecycleUpdate {
                    update: with_updated_at(subagent_event("running"), "2026-08-14T12:01:00Z"),
                    revision: stale_revision,
                },
            )
            .unwrap();
        // An ordered sentinel queued after the stale replay. Delivery is FIFO,
        // so receiving the sentinel third proves the stale replay was dropped
        // rather than merely delayed, without waiting on a wall-clock window.
        queue
            .try_send(&progress_tx, live(with_task_id(subagent_event("queued"), "sentinel-task".to_string())))
            .unwrap();
    });

    let first = duplex.next(Duration::from_secs(2)).await.expect("initial task update");
    let acp::AgentNotification::SessionNotification(first) = first else {
        panic!("task lifecycle must use standard session notifications");
    };
    let acp::SessionUpdate::ToolCall(call) = first.update else {
        panic!("first task snapshot must create a tool call");
    };
    assert_eq!(call.meta.expect("initial task metadata")["lody"]["task"]["status"], "in_progress");

    let second = duplex.next(Duration::from_secs(2)).await.expect("task progress update");
    let acp::AgentNotification::SessionNotification(second) = second else {
        panic!("task progress must remain a standard session notification");
    };
    let acp::SessionUpdate::ToolCallUpdate(update) = second.update else {
        panic!("later task snapshots must update the existing tool call");
    };
    assert_eq!(update.meta.expect("progress task metadata")["lody"]["task"]["status"], "completed");

    let sentinel = duplex.next(Duration::from_secs(2)).await.expect("ordered sentinel update");
    let acp::AgentNotification::SessionNotification(sentinel) = sentinel else {
        panic!("task lifecycle must use standard session notifications");
    };
    let acp::SessionUpdate::ToolCall(sentinel_call) = sentinel.update else {
        panic!("the sentinel task's first snapshot must create a tool call");
    };
    assert_eq!(
        sentinel_call.tool_call_id.0.as_ref(),
        "task:sentinel-task",
        "the stale replay queued before the sentinel must not have been forwarded"
    );
    assert_eq!(
        sentinel_call.meta.expect("sentinel task metadata")["lody"]["task"]["status"],
        "pending",
        "the sentinel must be the queued task's first snapshot"
    );
    assert!(
        duplex.received.try_recv().is_err(),
        "the sentinel must be the last queued update, so nothing may follow it"
    );

    duplex.finish().await;
}

#[tokio::test]
async fn forwards_lifecycle_events_through_the_official_acp_duplex() {
    let mut duplex = ForwarderDuplex::start("diapason-task-lifecycle-test", |progress_tx, queue| {
        queue
            .try_send(&progress_tx, live(subagent_event_for("other-session", "running")))
            .unwrap();
        queue.try_send(&progress_tx, live(subagent_event("running"))).unwrap();
        queue.try_send(&progress_tx, live(subagent_event("completed"))).unwrap();
    });

    let first = duplex.next(Duration::from_secs(2)).await.expect("initial task update");
    let acp::AgentNotification::SessionNotification(first) = first else {
        panic!("task lifecycle must use standard session notifications");
    };
    assert_eq!(first.session_id.0.as_ref(), "parent-session");
    let acp::SessionUpdate::ToolCall(call) = first.update else {
        panic!("first task snapshot must create a tool call");
    };
    assert_eq!(call.tool_call_id.0.as_ref(), "task:child-1");
    assert_eq!(call.meta.expect("initial task metadata")["lody"]["task"]["status"], "in_progress");

    let second = duplex.next(Duration::from_secs(2)).await.expect("task progress update");
    let acp::AgentNotification::SessionNotification(second) = second else {
        panic!("task progress must remain a standard session notification");
    };
    let acp::SessionUpdate::ToolCallUpdate(update) = second.update else {
        panic!("later task snapshots must update the existing tool call");
    };
    assert_eq!(update.tool_call_id.0.as_ref(), "task:child-1");
    assert_eq!(update.meta.expect("progress task metadata")["lody"]["task"]["status"], "completed");
    assert!(duplex.received.try_recv().is_err(), "foreign-session events must remain filtered");

    duplex.finish().await;
}

/// A live terminal update the full queue refused still reaches the client.
///
/// The attach-time snapshot pass can fill the bounded channel before the
/// forwarder drains it. A task that completes in that window is refused by the
/// full queue, and the client must still be told the task finished rather than
/// being left with the active snapshot this pass queued.
#[tokio::test]
async fn a_terminal_update_refused_by_a_full_queue_reaches_the_client() {
    let metrics = Arc::new(diapason_core::metrics::MetricsCollector::new());
    let queue = Arc::new(LifecycleQueueTracker::new(metrics));
    // One slot, so the snapshot the pass queued is what makes the refusal
    // reproducible before the forwarder has started draining.
    let (progress_tx, progress_rx) = mpsc::channel(1);
    let snapshot = live(subagent_event("running"));
    let terminal = live(with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"));
    assert!(queue.send(&progress_tx, snapshot).await.is_ok(), "the initial snapshot fills the only slot");
    assert_eq!(
        queue
            .try_send(&progress_tx, terminal)
            .expect_err("the full queue refuses the terminal update"),
        LifecycleQueueDropKind::Full
    );
    drop(progress_tx);

    let mut duplex = ForwarderDuplex::with_queue("diapason-task-refused-update-test", progress_rx, queue);
    let delivered = duplex.next(Duration::from_secs(2)).await.expect("recovered terminal update");
    let acp::AgentNotification::SessionNotification(delivered) = delivered else {
        panic!("task lifecycle must use standard session notifications");
    };
    let acp::SessionUpdate::ToolCall(call) = delivered.update else {
        panic!("the task's first delivered snapshot must create a tool call");
    };
    assert_eq!(call.tool_call_id.0.as_ref(), "task:child-1");
    assert_eq!(
        call.meta.expect("terminal task metadata")["lody"]["task"]["status"],
        "completed",
        "the refused update must be recovered so the client sees the terminal state"
    );
    assert!(
        duplex.next(Duration::from_millis(100)).await.is_none(),
        "the active snapshot the terminal state superseded must not follow it"
    );

    duplex.finish().await;
}
