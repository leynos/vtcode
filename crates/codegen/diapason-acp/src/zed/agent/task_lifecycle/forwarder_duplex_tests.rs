//! Duplex transport regressions for the lifecycle forwarder.
use super::projection_tests::{background_event, subagent_event, subagent_event_for, with_updated_at};
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
                    let metrics = Arc::new(diapason_core::metrics::MetricsCollector::new());
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
    let mut duplex = ForwarderDuplex::start("diapason-task-ordering-test", |progress_tx, queue| {
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
    let mut duplex = ForwarderDuplex::start("diapason-task-lifecycle-test", |progress_tx, queue| {
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

/// Log sink that retains formatted records so a test can inspect them.
#[derive(Clone, Default)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

impl CapturedLogs {
    /// The captured output as text.
    fn text(&self) -> String {
        let bytes = self.0.lock().expect("captured log mutex").clone();
        String::from_utf8(bytes).expect("captured logs are UTF-8")
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("captured log mutex").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'writer> tracing_subscriber::fmt::MakeWriter<'writer> for CapturedLogs {
    type Writer = Self;

    fn make_writer(&'writer self) -> Self::Writer {
        self.clone()
    }
}

/// A superseded snapshot is dropped without logging its identifiers.
///
/// The forwarder orders task revisions, so a late snapshot that cannot
/// supersede the forwarded one is discarded rather than rolling the client's
/// task back. That diagnostic is the only place the dropped task is reported,
/// and the task and session identifiers are client-visible values: the record
/// must carry a bounded correlation instead of either one.
/// A background task whose identifiers embed the session id it belongs to.
///
/// A dropped-update diagnostic must not leak either identifier, so the fixture
/// makes the task id contain the session id: a correlation derived from the
/// task id would still hide it, and logging the raw id would reveal both.
fn background_event_with_session_in_id(status: &str) -> LifecycleUpdate {
    let mut event = background_event(status);
    if let LifecycleUpdate::BackgroundProcess { task, .. } = &mut event {
        task.id = "bg-parent-session-watcher".to_string();
        task.session_id = "parent-session".to_string();
        task.exec_session_id = "exec-parent-session-watcher".to_string();
    }
    event
}

#[tokio::test]
async fn superseded_task_update_logs_a_bounded_correlation() {
    let logs = CapturedLogs::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(logs.clone())
            .finish(),
    );

    let mut duplex = ForwarderDuplex::start("diapason-task-correlation-test", |progress_tx, queue| {
        queue
            .try_send(&progress_tx, with_updated_at(subagent_event("completed"), "2026-08-14T12:05:00Z"))
            .expect("enqueue the settled task update");
        // The same revision instant, but not terminal: a replay artefact
        // rather than a recorded transition, so the forwarder drops it.
        queue
            .try_send(&progress_tx, with_updated_at(subagent_event("running"), "2026-08-14T12:05:00Z"))
            .expect("enqueue the superseded task update");
        queue
            .try_send(
                &progress_tx,
                with_updated_at(background_event_with_session_in_id("running"), "2026-08-14T12:05:00Z"),
            )
            .expect("enqueue the settled background update");
        // The background task id embeds its session id, so a raw id in the
        // dropped-update record would leak the session as well.
        queue
            .try_send(
                &progress_tx,
                with_updated_at(background_event_with_session_in_id("running"), "2026-08-14T12:01:00Z"),
            )
            .expect("enqueue the superseded background update");
    });

    let settled = duplex.next(Duration::from_secs(2)).await.expect("settled task update");
    assert!(
        matches!(settled, acp::AgentNotification::SessionNotification(_)),
        "task lifecycle stays on session updates"
    );
    let background = duplex.next(Duration::from_secs(2)).await.expect("settled background update");
    assert!(
        matches!(background, acp::AgentNotification::SessionNotification(_)),
        "background lifecycle stays on session updates"
    );
    let stale = duplex.next(Duration::from_millis(250)).await;
    assert!(stale.is_none(), "a superseded snapshot must not be forwarded");

    duplex.finish().await;

    let captured = logs.text();
    let records: Vec<&str> = captured
        .lines()
        .filter(|line| line.contains("Dropped superseded ACP task lifecycle update"))
        .collect();
    assert_eq!(records.len(), 2, "the forwarder must report each dropped update: {captured}");
    for record in records {
        assert!(record.contains("task_correlation="), "the diagnostic must carry a bounded correlation: {record}");
        assert!(!record.contains("child-1"), "the task id must not reach the log: {record}");
        assert!(
            !record.contains("bg-parent-session-watcher"),
            "the background task id must not reach the log: {record}"
        );
        assert!(!record.contains("parent-session"), "the session id must not reach the log: {record}");
    }
}
