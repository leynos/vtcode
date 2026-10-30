//! ACP lifecycle notification payload construction and serialization.

use super::TASK_LIFECYCLE_METHOD;
use crate::acp;
use crate::zed::connection::ConnectionHandle;
use serde_json::{Value, json};
use std::sync::Arc;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentStatus, SubagentStatusEntry,
};

pub(super) enum LifecycleUpdate {
    Subagent(SubagentStatusEntry),
    BackgroundProcess(BackgroundSubprocessEntry),
}

pub(super) fn send_task_lifecycle(
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

pub(super) fn lifecycle_message(update: LifecycleUpdate) -> anyhow::Result<Value> {
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
