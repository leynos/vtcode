//! Duplex transport regressions for the lifecycle forwarder.
use super::projection_tests::{subagent_event, subagent_event_for, with_updated_at};
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
        publish: impl FnOnce(mpsc::Sender<LifecycleUpdate>, Arc<LifecycleQueueTracker>) + Send + 'static,
    ) -> Self {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let (progress_tx, progress_rx) = mpsc::channel(8);
        let (received_tx, received) = mpsc::unbounded_channel();

        let agent_connection =
            Agent
                .builder()
                .name(agent_name)
                .connect_with(agent_channel, move |cx: ConnectionTo<Client>| async move {
                    let client = ConnectionHandle::new(cx);
                    let metrics = Arc::new(vtcode_core::metrics::MetricsCollector::new());
                    let queue = Arc::new(LifecycleQueueTracker::new(metrics));
                    let forwarder = spawn_task_lifecycle_forwarder(
                        progress_rx,
                        acp::SessionId::new(Arc::from("parent-session")),
                        client,
                        Arc::clone(&queue),
                    );
                    publish(progress_tx, queue);
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
    let mut duplex = ForwarderDuplex::start("vtcode-task-ordering-test", |progress_tx, queue| {
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

    let stale = duplex.next(Duration::from_millis(250)).await;
    assert!(stale.is_none(), "a stale initial snapshot must not be forwarded after settlement");

    duplex.finish().await;
}

#[tokio::test]
async fn forwards_lifecycle_events_through_the_official_acp_duplex() {
    let mut duplex = ForwarderDuplex::start("vtcode-task-lifecycle-test", |progress_tx, queue| {
        queue
            .try_send(&progress_tx, subagent_event_for("other-session", "running"))
            .unwrap();
        queue.try_send(&progress_tx, subagent_event("running")).unwrap();
        queue.try_send(&progress_tx, subagent_event("completed")).unwrap();
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
