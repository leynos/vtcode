//! Regression coverage for lifecycle queueing, mapping, and ACP forwarding.

use super::messages::lifecycle_message;
use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use serde_json::Value;
use serde_json::json;
use tokio::sync::mpsc;

#[test]
fn classifies_lifecycle_queue_full_and_closed_errors() {
    let (sender, _receiver) = mpsc::channel::<()>(1);
    sender.try_send(()).expect("first send fits in the queue");
    let full_error = sender.try_send(()).expect_err("second send should find a full queue");
    assert_eq!(lifecycle_queue_drop_kind(full_error), LifecycleQueueDropKind::Full);

    let (sender, receiver) = mpsc::channel::<()>(1);
    drop(receiver);
    let closed_error = sender.try_send(()).expect_err("closed receiver should reject a send");
    assert_eq!(lifecycle_queue_drop_kind(closed_error), LifecycleQueueDropKind::Closed);
}

#[test]
fn lifecycle_queue_metrics_track_depth_and_bounded_drop_categories() {
    let metrics = Arc::new(MetricsCollector::new());
    let queue = LifecycleQueueTracker::new(Arc::clone(&metrics));
    let (sender, mut receiver) = mpsc::channel(1);

    queue.try_send(&sender, ()).expect("first queue entry should fit");
    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_depth, 1);
    let full = queue
        .try_send(&sender, ())
        .expect_err("second queue entry should find a full queue");
    assert_eq!(full, LifecycleQueueDropKind::Full);
    assert_eq!(queue.record_drop(full), 1);

    receiver.try_recv().expect("first queue entry should be available");
    queue.dequeued();
    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_depth, 0);

    drop(receiver);
    let closed = queue.try_send(&sender, ()).expect_err("closed queue should reject an entry");
    assert_eq!(closed, LifecycleQueueDropKind::Closed);
    assert_eq!(queue.record_drop(closed), 1);

    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_dropped_full, 1);
    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_dropped_closed, 1);
}

#[test]
fn lifecycle_forwarder_shutdown_drains_queued_depth_before_closing_tracker() {
    let metrics = Arc::new(MetricsCollector::new());
    let queue = LifecycleQueueTracker::new(Arc::clone(&metrics));
    let (sender, _receiver) = mpsc::channel(1);
    queue.try_send(&sender, ()).expect("queue entry should fit");
    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_depth, 1);

    queue.close_and_drain();

    assert_eq!(metrics.get_acp_task_lifecycle_metrics().queue_depth, 0);
    assert_eq!(
        queue
            .try_send(&sender, ())
            .expect_err("closed tracker should reject new entries"),
        LifecycleQueueDropKind::Closed
    );
}

#[test]
fn lifecycle_forwarder_outcome_is_bounded_by_send_result() {
    assert_eq!(task_lifecycle_forwarder_outcome(0), "completed");
    assert_eq!(task_lifecycle_forwarder_outcome(1), "send_failed");
}

fn subagent_entry(status: &str) -> SubagentStatusEntry {
    serde_json::from_value(json!({
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
    .expect("valid subagent status fixture")
}

fn background_entry(status: &str) -> BackgroundSubprocessEntry {
    serde_json::from_value(json!({
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
    .expect("valid background status fixture")
}

#[test]
fn maps_worker_and_background_states_to_claude_style_lifecycle_messages() {
    assert_eq!(
        lifecycle_message(LifecycleUpdate::Subagent(subagent_entry("queued"))).expect("queued message")["type"],
        "task_started"
    );
    assert_eq!(
        lifecycle_message(LifecycleUpdate::Subagent(subagent_entry("running"))).expect("running message")["type"],
        "task_progress"
    );
    let failed = lifecycle_message(LifecycleUpdate::Subagent(subagent_entry("failed"))).expect("failed message");
    assert_eq!(failed["type"], "task_updated");
    assert_eq!(failed["patch"]["status"], "failed");

    let background =
        lifecycle_message(LifecycleUpdate::BackgroundProcess(background_entry("running"))).expect("background message");
    assert_eq!(background["type"], "task_progress");
    assert_eq!(background["task_type"], "background_process");
    assert_eq!(background["details"]["exec_session_id"], "exec-background-1");
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
            let forwarder = spawn_task_lifecycle_forwarder(
                progress_rx,
                acp::SessionId::new(Arc::from("parent-session")),
                client,
                Arc::new(LifecycleQueueTracker::new(Arc::new(MetricsCollector::new()))),
            );
            progress_tx
                .send(LifecycleUpdate::Subagent(subagent_entry("running")))
                .await
                .expect("queue subagent update");
            progress_tx
                .send(LifecycleUpdate::BackgroundProcess(background_entry("running")))
                .await
                .expect("queue background update");
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
                if let acp::AgentNotification::ExtNotification(notification) = notification {
                    drop(received_tx.send(notification));
                }
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |_cx: ConnectionTo<Agent>| {
            let notification = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("extension notification deadline")
                .expect("extension notification");
            // The official SDK strips the required leading underscore
            // while decoding extension methods; the wire method remains
            // `_vtcode/taskLifecycle`.
            assert_eq!(notification.method.as_ref(), TASK_LIFECYCLE_METHOD.trim_start_matches('_'));
            let payload: Value = serde_json::from_str(notification.params.get()).expect("extension payload");
            assert_eq!(payload["sessionId"], "parent-session");
            assert_eq!(payload["acpSessionId"], "parent-session");
            assert_eq!(payload["message"]["type"], "task_progress");
            assert_eq!(payload["message"]["task_id"], "child-1");
            let notification = tokio::time::timeout(std::time::Duration::from_secs(2), received_rx.recv())
                .await
                .expect("background extension notification deadline")
                .expect("background extension notification");
            let payload: Value = serde_json::from_str(notification.params.get()).expect("extension payload");
            assert_eq!(payload["message"]["task_type"], "background_process");
            assert_eq!(payload["message"]["task_id"], "background-1");
            Ok(())
        });

    tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
        .await
        .expect("client connection deadline")
        .expect("client connection");
    agent_task.await.expect("agent task").expect("agent connection");
}
