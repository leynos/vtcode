use super::super::types::SessionHandle;
use super::ZedAgent;
use super::lody::{lody_task_id, lody_task_session_update};
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use hashbrown::HashSet;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::warn;
use vtcode_core::subagents::SubagentProgressEvent;

impl ZedAgent {
    pub(super) fn ensure_task_lifecycle_forwarder(&self, session: &SessionHandle) {
        let already_running = session
            .data
            .lock()
            .map(|data| data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()))
            .unwrap_or(true);
        if already_running {
            return;
        }

        let Some(controller) = self.session_subagent_controller(session) else {
            return;
        };
        let Some(client) = self.client() else {
            return;
        };
        let session_id = session
            .data
            .lock()
            .map(|data| data.session_id.clone())
            .unwrap_or_else(|error| error.into_inner().session_id.clone());
        let receiver = controller.subscribe_progress();
        let forwarder = spawn_task_lifecycle_forwarder(receiver, session_id, client);

        if let Ok(mut data) = session.data.lock() {
            if data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()) {
                forwarder.abort();
            } else {
                data.task_lifecycle_forwarder = Some(forwarder);
            }
        } else {
            forwarder.abort();
        }
    }
}

fn spawn_task_lifecycle_forwarder(
    mut receiver: broadcast::Receiver<SubagentProgressEvent>,
    session_id: acp::SessionId,
    client: Arc<ConnectionHandle>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut emitted_task_ids = HashSet::new();
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.parent_session_id() != session_id.0.as_ref() {
                        continue;
                    }
                    let task_id = lody_task_id(&event).to_string();
                    let previously_emitted = emitted_task_ids.contains(&task_id);
                    if let Err(error) = send_task_lifecycle(&client, &session_id, event, previously_emitted) {
                        warn!(%error, %session_id, "Failed to forward ACP task lifecycle notification");
                    } else {
                        let _ = emitted_task_ids.insert(task_id);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(%session_id, skipped, "ACP task lifecycle receiver lagged; continuing from latest state");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    })
}

fn send_task_lifecycle(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    event: SubagentProgressEvent,
    previously_emitted: bool,
) -> anyhow::Result<()> {
    let update = lody_task_session_update(event, previously_emitted)?;
    client
        .send_session_notification(acp::SessionNotification::new(session_id.clone(), update))
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
    use proptest::prelude::*;
    use serde_json::{Value, json};
    use tokio::sync::mpsc;

    fn subagent_event(status: &str) -> SubagentProgressEvent {
        subagent_event_for("parent-session", status)
    }

    fn subagent_event_for(parent_session_id: &str, status: &str) -> SubagentProgressEvent {
        SubagentProgressEvent::Subagent {
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

    fn background_event(status: &str) -> SubagentProgressEvent {
        SubagentProgressEvent::BackgroundProcess {
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

    fn with_task_id(mut event: SubagentProgressEvent, task_id: String) -> SubagentProgressEvent {
        match &mut event {
            SubagentProgressEvent::Subagent { task, .. } => task.id = task_id,
            SubagentProgressEvent::BackgroundProcess { task, .. } => task.id = task_id,
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

    proptest! {
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

    #[tokio::test]
    async fn forwards_lifecycle_events_through_the_official_acp_duplex() {
        let (agent_channel, client_channel) = agent_client_protocol::Channel::duplex();
        let (progress_tx, progress_rx) = broadcast::channel(8);
        let (received_tx, mut received_rx) = mpsc::unbounded_channel();

        let agent_connection = Agent.builder().name("vtcode-task-lifecycle-test").connect_with(
            agent_channel,
            move |cx: ConnectionTo<Client>| async move {
                let client = ConnectionHandle::new(cx);
                let forwarder = spawn_task_lifecycle_forwarder(
                    progress_rx,
                    acp::SessionId::new(Arc::from("parent-session")),
                    client,
                );
                drop(progress_tx.send(subagent_event_for("other-session", "running")));
                drop(progress_tx.send(subagent_event("running")));
                drop(progress_tx.send(subagent_event("completed")));
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
}
