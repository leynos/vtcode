use super::super::types::SessionHandle;
use super::ZedAgent;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use tracing::warn;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentProgressEvent, SubagentStatus, SubagentStatusEntry,
};

pub(super) const TASK_LIFECYCLE_METHOD: &str = "_vtcode/taskLifecycle";

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
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    if event.parent_session_id() != session_id.0.as_ref() {
                        continue;
                    }
                    if let Err(error) = send_task_lifecycle(&client, &session_id, event) {
                        warn!(%error, %session_id, "Failed to forward ACP task lifecycle notification");
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
) -> anyhow::Result<()> {
    let message = lifecycle_message(event)?;
    let payload = json!({
        "sessionId": session_id.to_string(),
        "acpSessionId": session_id.to_string(),
        "message": message,
    });
    let params: Arc<serde_json::value::RawValue> = serde_json::value::to_raw_value(&payload)?.into();
    client
        .send_ext_notification(acp::ExtNotification::new(TASK_LIFECYCLE_METHOD, params))
        .map_err(|error| anyhow::anyhow!(error.to_string()))
}

fn lifecycle_message(event: SubagentProgressEvent) -> anyhow::Result<Value> {
    match event {
        SubagentProgressEvent::Subagent { task, .. } => subagent_lifecycle_message(task),
        SubagentProgressEvent::BackgroundProcess { task, .. } => background_lifecycle_message(task),
    }
}

fn subagent_lifecycle_message(entry: SubagentStatusEntry) -> anyhow::Result<Value> {
    let message_type = lifecycle_message_type(entry.status.is_terminal(), entry.status == SubagentStatus::Queued);
    let status = terminal_subagent_status(entry.status);
    lifecycle_message_value(message_type, "subagent", entry.id.clone(), status, entry.agent_name.clone(), entry)
}

fn background_lifecycle_message(entry: BackgroundSubprocessEntry) -> anyhow::Result<Value> {
    let terminal = matches!(entry.status, BackgroundSubprocessStatus::Stopped | BackgroundSubprocessStatus::Error);
    let message_type = lifecycle_message_type(terminal, entry.status == BackgroundSubprocessStatus::Starting);
    let status = match entry.status {
        BackgroundSubprocessStatus::Starting => "pending",
        BackgroundSubprocessStatus::Running => "in_progress",
        BackgroundSubprocessStatus::Stopped => "completed",
        BackgroundSubprocessStatus::Error => "failed",
    };
    lifecycle_message_value(
        message_type,
        "background_process",
        entry.id.clone(),
        status,
        entry.agent_name.clone(),
        entry,
    )
}

fn lifecycle_message_type(terminal: bool, starting: bool) -> &'static str {
    if terminal {
        "task_updated"
    } else if starting {
        "task_started"
    } else {
        "task_progress"
    }
}

fn terminal_subagent_status(status: SubagentStatus) -> &'static str {
    match status {
        SubagentStatus::Queued => "pending",
        SubagentStatus::Running | SubagentStatus::Waiting => "in_progress",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Closed => "killed",
    }
}

fn lifecycle_message_value(
    message_type: &str,
    task_type: &str,
    task_id: String,
    status: &str,
    agent_name: String,
    details: impl serde::Serialize,
) -> anyhow::Result<Value> {
    let details = serde_json::to_value(details)?;
    if message_type == "task_updated" {
        Ok(json!({
            "type": message_type,
            "task_id": task_id,
            "task_type": task_type,
            "subagent_type": agent_name,
            "patch": { "status": status, "details": details },
        }))
    } else {
        Ok(json!({
            "type": message_type,
            "task_id": task_id,
            "task_type": task_type,
            "subagent_type": agent_name,
            "status": status,
            "details": details,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
    use serde_json::json;
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

    #[test]
    fn maps_worker_and_background_states_to_claude_style_lifecycle_messages() {
        assert_eq!(lifecycle_message(subagent_event("queued")).expect("queued message")["type"], "task_started");
        assert_eq!(lifecycle_message(subagent_event("running")).expect("running message")["type"], "task_progress");
        let failed = lifecycle_message(subagent_event("failed")).expect("failed message");
        assert_eq!(failed["type"], "task_updated");
        assert_eq!(failed["patch"]["status"], "failed");

        let background = lifecycle_message(background_event("running")).expect("background message");
        assert_eq!(background["type"], "task_progress");
        assert_eq!(background["task_type"], "background_process");
        assert_eq!(background["details"]["exec_session_id"], "exec-background-1");
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
                Ok(())
            });

        tokio::time::timeout(std::time::Duration::from_secs(3), client_connection)
            .await
            .expect("client connection deadline")
            .expect("client connection");
        agent_task.await.expect("agent task").expect("agent connection");
    }
}
