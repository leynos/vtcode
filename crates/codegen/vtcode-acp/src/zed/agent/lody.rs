use crate::acp;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Map, Value};
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentProgressEvent, SubagentStatus, SubagentStatusEntry,
};

const LODY_EXTENSION_VERSION: u8 = 1;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LodyTaskMeta<'a> {
    version: u8,
    task_id: &'a str,
    kind: &'static str,
    status: &'static str,
    description: &'a str,
    actor: &'a str,
    started_at_epoch_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    ended_at_epoch_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

struct TaskSnapshot<'a> {
    id: &'a str,
    title: &'a str,
    status: acp::ToolCallStatus,
    meta: LodyTaskMeta<'a>,
}

pub(super) fn lody_task_id(event: &SubagentProgressEvent) -> &str {
    match event {
        SubagentProgressEvent::Subagent { task, .. } => &task.id,
        SubagentProgressEvent::BackgroundProcess { task, .. } => &task.id,
    }
}

pub(super) fn lody_task_session_update(
    event: SubagentProgressEvent,
    previously_emitted: bool,
) -> anyhow::Result<acp::SessionUpdate> {
    let snapshot = match &event {
        SubagentProgressEvent::Subagent { task, .. } => subagent_snapshot(task),
        SubagentProgressEvent::BackgroundProcess { task, .. } => background_snapshot(task),
    };
    let tool_call_id = format!("task:{}", snapshot.id);
    let task_value = serde_json::to_value(snapshot.meta)?;
    let mut lody = Map::new();
    let _ = lody.insert("task".to_string(), task_value);
    let mut meta = Map::new();
    let _ = meta.insert("lody".to_string(), Value::Object(lody));

    if previously_emitted {
        let fields = acp::ToolCallUpdateFields::new()
            .title(snapshot.title.to_string())
            .kind(acp::ToolKind::Think)
            .status(snapshot.status)
            .content(task_content(&event));
        Ok(acp::SessionUpdate::ToolCallUpdate(acp::ToolCallUpdate::new(tool_call_id, fields).meta(meta)))
    } else {
        Ok(acp::SessionUpdate::ToolCall(
            acp::ToolCall::new(tool_call_id, snapshot.title)
                .kind(acp::ToolKind::Think)
                .status(snapshot.status)
                .content(task_content(&event))
                .meta(meta),
        ))
    }
}

pub(super) fn add_lody_subagent_lifecycle_capability(capabilities: &mut acp::AgentCapabilities) {
    let mut subagents = Map::new();
    let _ = subagents.insert("version".to_string(), Value::from(LODY_EXTENSION_VERSION));
    let _ = subagents.insert("lifecycle".to_string(), Value::Bool(true));

    let mut lody = Map::new();
    let _ = lody.insert("subagents".to_string(), Value::Object(subagents));

    let meta = capabilities.meta.get_or_insert_with(Map::new);
    let _ = meta.insert("lody".to_string(), Value::Object(lody));
}

fn subagent_snapshot(entry: &SubagentStatusEntry) -> TaskSnapshot<'_> {
    let (lody_status, acp_status) = match entry.status {
        SubagentStatus::Queued => ("pending", acp::ToolCallStatus::Pending),
        SubagentStatus::Running | SubagentStatus::Waiting => ("in_progress", acp::ToolCallStatus::InProgress),
        SubagentStatus::Completed => ("completed", acp::ToolCallStatus::Completed),
        SubagentStatus::Failed | SubagentStatus::Closed => ("failed", acp::ToolCallStatus::Failed),
    };
    TaskSnapshot {
        id: &entry.id,
        title: &entry.display_label,
        status: acp_status,
        meta: LodyTaskMeta {
            version: LODY_EXTENSION_VERSION,
            task_id: &entry.id,
            kind: "subagent",
            status: lody_status,
            description: &entry.description,
            actor: &entry.agent_name,
            started_at_epoch_seconds: epoch_seconds(entry.created_at),
            ended_at_epoch_seconds: entry.completed_at.map(epoch_seconds),
            summary: entry.summary.as_deref(),
            error: entry.error.as_deref(),
        },
    }
}

fn background_snapshot(entry: &BackgroundSubprocessEntry) -> TaskSnapshot<'_> {
    let (lody_status, acp_status) = match entry.status {
        BackgroundSubprocessStatus::Starting => ("pending", acp::ToolCallStatus::Pending),
        BackgroundSubprocessStatus::Running => ("in_progress", acp::ToolCallStatus::InProgress),
        BackgroundSubprocessStatus::Stopped => ("completed", acp::ToolCallStatus::Completed),
        BackgroundSubprocessStatus::Error => ("failed", acp::ToolCallStatus::Failed),
    };
    TaskSnapshot {
        id: &entry.id,
        title: &entry.display_label,
        status: acp_status,
        meta: LodyTaskMeta {
            version: LODY_EXTENSION_VERSION,
            task_id: &entry.id,
            kind: "background",
            status: lody_status,
            description: &entry.description,
            actor: &entry.agent_name,
            started_at_epoch_seconds: epoch_seconds(entry.started_at.unwrap_or(entry.created_at)),
            ended_at_epoch_seconds: entry.ended_at.map(epoch_seconds),
            summary: entry.summary.as_deref(),
            error: entry.error.as_deref(),
        },
    }
}

fn epoch_seconds(timestamp: DateTime<Utc>) -> f64 {
    timestamp.timestamp_millis().max(0) as f64 / 1_000.0
}

fn task_content(event: &SubagentProgressEvent) -> Vec<acp::ToolCallContent> {
    let text = match event {
        SubagentProgressEvent::Subagent { task, .. } => {
            task.error.as_deref().or(task.summary.as_deref()).unwrap_or(&task.description)
        }
        SubagentProgressEvent::BackgroundProcess { task, .. } => {
            task.error.as_deref().or(task.summary.as_deref()).unwrap_or(&task.description)
        }
    };
    vec![acp::ContentBlock::Text(acp::TextContent::new(text)).into()]
}
