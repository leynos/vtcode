//! Standard ACP lifecycle projection and transport regressions.
use super::super::lody::lody_task_session_update;
use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use chrono::{DateTime, Utc};
use proptest::prelude::*;
use serde_json::{Value, json};
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
