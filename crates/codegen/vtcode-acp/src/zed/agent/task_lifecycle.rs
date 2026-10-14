//! Forward sub-agent and background-process lifecycle events to ACP clients.
//!
//! A session-scoped observer receives domain status snapshots and sends them
//! as VTCode task-lifecycle notifications.

use super::super::types::{SessionHandle, lock_poison_error};
use super::ZedAgent;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentProgressObserver, SubagentStatus,
    SubagentStatusEntry,
};

pub(super) const TASK_LIFECYCLE_METHOD: &str = "_vtcode/taskLifecycle";

impl ZedAgent {
    pub(super) fn ensure_task_lifecycle_forwarder(&self, session: &SessionHandle) -> anyhow::Result<()> {
        let already_running = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("checking ACP task lifecycle forwarder state", error))?
            .task_lifecycle_forwarder
            .as_ref()
            .is_some_and(|task| !task.is_finished());
        if already_running {
            return Ok(());
        }

        let Some(controller) = self.session_subagent_controller(session)? else {
            return Ok(());
        };
        let Some(client) = self.client() else {
            return Ok(());
        };
        let session_id = session
            .data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session ID for task lifecycle forwarding", error))?
            .session_id
            .clone();
        let (sender, receiver) = mpsc::channel(128);
        let observer = Arc::new(AcpTaskLifecycleObserver { sender, session_id: session_id.clone() });
        let registration = controller.register_progress_observer(session_id.0.to_string(), observer);
        let forwarder = spawn_task_lifecycle_forwarder(receiver, session_id, client);

        let mut data = match session
            .data
            .lock()
            .map_err(|error| lock_poison_error("registering ACP task lifecycle forwarder", error))
        {
            Ok(data) => data,
            Err(error) => {
                forwarder.abort();
                return Err(error);
            }
        };
        if data.task_lifecycle_forwarder.as_ref().is_some_and(|task| !task.is_finished()) {
            forwarder.abort();
        } else {
            data.task_lifecycle_forwarder = Some(forwarder);
            data.task_lifecycle_registration = Some(registration);
        }
        Ok(())
    }
}

enum LifecycleUpdate {
    Subagent(SubagentStatusEntry),
    BackgroundProcess(BackgroundSubprocessEntry),
}

struct AcpTaskLifecycleObserver {
    sender: mpsc::Sender<LifecycleUpdate>,
    session_id: acp::SessionId,
}

impl SubagentProgressObserver for AcpTaskLifecycleObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.try_send(LifecycleUpdate::Subagent(entry));
    }

    fn background_status_changed(&self, entry: BackgroundSubprocessEntry) {
        self.try_send(LifecycleUpdate::BackgroundProcess(entry));
    }
}

impl AcpTaskLifecycleObserver {
    fn try_send(&self, update: LifecycleUpdate) {
        if let Err(error) = self.sender.try_send(update) {
            warn!(%error, session_id = %self.session_id, "ACP task lifecycle update could not be queued");
        }
    }
}

fn spawn_task_lifecycle_forwarder(
    mut receiver: mpsc::Receiver<LifecycleUpdate>,
    session_id: acp::SessionId,
    client: Arc<ConnectionHandle>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        while let Some(update) = receiver.recv().await {
            if let Err(error) = send_task_lifecycle(&client, &session_id, update) {
                warn!(%error, %session_id, "Failed to forward ACP task lifecycle notification");
            }
        }
    })
}

fn send_task_lifecycle(
    client: &ConnectionHandle,
    session_id: &acp::SessionId,
    update: LifecycleUpdate,
) -> anyhow::Result<()> {
    let message = lifecycle_message(update)?;
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

fn lifecycle_message(update: LifecycleUpdate) -> anyhow::Result<Value> {
    match update {
        LifecycleUpdate::Subagent(entry) => subagent_lifecycle_message(entry),
        LifecycleUpdate::BackgroundProcess(entry) => background_lifecycle_message(entry),
    }
}

fn subagent_lifecycle_message(entry: SubagentStatusEntry) -> anyhow::Result<Value> {
    let message_type = lifecycle_message_type(entry.status.is_terminal(), entry.status == SubagentStatus::Queued);
    let status = terminal_subagent_status(entry.status);
    lifecycle_message_value(LifecycleMessageFields {
        message_type,
        task_type: "subagent",
        task_id: entry.id.clone(),
        status,
        agent_name: entry.agent_name.clone(),
        details: entry,
    })
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
    lifecycle_message_value(LifecycleMessageFields {
        message_type,
        task_type: "background_process",
        task_id: entry.id.clone(),
        status,
        agent_name: entry.agent_name.clone(),
        details: entry,
    })
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

struct LifecycleMessageFields<Details> {
    message_type: &'static str,
    task_type: &'static str,
    task_id: String,
    status: &'static str,
    agent_name: String,
    details: Details,
}

fn lifecycle_message_value(fields: LifecycleMessageFields<impl serde::Serialize>) -> anyhow::Result<Value> {
    let LifecycleMessageFields {
        message_type,
        task_type,
        task_id,
        status,
        agent_name,
        details,
    } = fields;
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

        let background = lifecycle_message(LifecycleUpdate::BackgroundProcess(background_entry("running")))
            .expect("background message");
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
}
