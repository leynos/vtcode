use crate::acp;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::HashSet;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentProgressEvent, SubagentStatus, SubagentStatusEntry,
    extract_tail_lines, normalize_output_tail_lines,
};

use super::ZedAgent;
use agent_client_protocol::Error as SdkError;

const LODY_EXTENSION_VERSION: u8 = 1;
pub(super) const LODY_SUBAGENTS_LIST_METHOD: &str = "_lody/subagents/list";
pub(super) const LODY_SUBAGENTS_CANCEL_METHOD: &str = "_lody/subagents/cancel";
pub(super) const LODY_SUBAGENTS_OUTPUT_METHOD: &str = "_lody/subagents/output";

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

pub(super) fn add_lody_subagent_management_capability(
    capabilities: &mut acp::AgentCapabilities,
    background_enabled: bool,
) {
    add_lody_subagent_lifecycle_capability(capabilities);
    let Some(Value::Object(lody)) = capabilities.meta.as_mut().and_then(|meta| meta.get_mut("lody")) else {
        return;
    };
    let Some(Value::Object(subagents)) = lody.get_mut("subagents") else {
        return;
    };
    for operation in ["list", "cancel", "output"] {
        let _ = subagents.insert(operation.to_string(), Value::Bool(true));
    }
    if background_enabled {
        let mut tasks = Map::new();
        let _ = tasks.insert("version".to_string(), Value::from(LODY_EXTENSION_VERSION));
        let _ = tasks.insert("background".to_string(), Value::Bool(true));
        let _ = lody.insert("tasks".to_string(), Value::Object(tasks));
    }
}

pub(super) fn is_lody_subagent_management_method(method: &str) -> bool {
    matches!(method, LODY_SUBAGENTS_LIST_METHOD | LODY_SUBAGENTS_CANCEL_METHOD | LODY_SUBAGENTS_OUTPUT_METHOD)
}

pub(super) async fn handle_lody_subagent_management(
    agent: &ZedAgent,
    method: &str,
    params: Value,
) -> Result<Value, SdkError> {
    match method {
        LODY_SUBAGENTS_LIST_METHOD => list_subagents(agent, parse_params(params)?).await,
        LODY_SUBAGENTS_CANCEL_METHOD => cancel_subagent(agent, parse_params(params)?).await,
        LODY_SUBAGENTS_OUTPUT_METHOD => subagent_output(agent, parse_params(params)?).await,
        _ => Err(SdkError::method_not_found()),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ListSubagentsRequest {
    session_id: String,
    #[serde(default)]
    active_only: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelSubagentRequest {
    session_id: String,
    task_id: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SubagentOutputRequest {
    session_id: String,
    task_id: String,
    tail: Option<usize>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManagedTask {
    task_id: String,
    description: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_type: Option<String>,
    started_at_epoch_seconds: f64,
    ended_at_epoch_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
}

fn parse_params<T: for<'de> Deserialize<'de>>(params: Value) -> Result<T, SdkError> {
    serde_json::from_value(params).map_err(|error| {
        SdkError::invalid_params().data(serde_json::json!({
            "reason": "invalid_lody_subagent_params",
            "detail": error.to_string(),
        }))
    })
}

fn session_controller(
    agent: &ZedAgent,
    session_id: &str,
) -> Result<(super::super::types::SessionHandle, std::sync::Arc<vtcode_core::subagents::SubagentController>), SdkError>
{
    let session_id = acp::SessionId::new(session_id);
    let session = agent
        .session_handle(&session_id)
        .ok_or_else(|| SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_session" })))?;
    let controller = agent.session_subagent_controller(&session).ok_or_else(|| {
        SdkError::invalid_params().data(serde_json::json!({ "reason": "subagent_management_unavailable" }))
    })?;
    Ok((session, controller))
}

async fn list_subagents(agent: &ZedAgent, request: ListSubagentsRequest) -> Result<Value, SdkError> {
    let (_session, controller) = session_controller(agent, &request.session_id)?;
    let children = controller.status_entries().await;
    let owned_children = owned_child_ids(&request.session_id, &children);
    let mut tasks = children
        .iter()
        .filter(|entry| owned_children.contains(entry.id.as_str()))
        .filter(|entry| !request.active_only || !entry.status.is_terminal())
        .map(managed_child_task)
        .collect::<Vec<_>>();
    tasks.extend(
        controller
            .background_status_entries()
            .await
            .iter()
            .filter(|entry| entry.owner_session_id.as_deref() == Some(request.session_id.as_str()))
            .filter(|entry| !request.active_only || entry.status.is_active())
            .map(managed_background_task),
    );
    tasks.sort_unstable_by(|left, right| left.task_id.cmp(&right.task_id));
    Ok(serde_json::json!({ "tasks": tasks }))
}

async fn cancel_subagent(agent: &ZedAgent, request: CancelSubagentRequest) -> Result<Value, SdkError> {
    let (_session, controller) = session_controller(agent, &request.session_id)?;
    let children = controller.status_entries().await;
    if owned_child_ids(&request.session_id, &children).contains(request.task_id.as_str()) {
        drop(controller.close(&request.task_id).await.map_err(internal_management_error)?);
        return Ok(serde_json::json!({}));
    }
    let owned_background = controller.background_status_entries().await.into_iter().any(|entry| {
        entry.id == request.task_id && entry.owner_session_id.as_deref() == Some(request.session_id.as_str())
    });
    if owned_background {
        drop(
            controller
                .force_cancel_background(&request.task_id)
                .await
                .map_err(internal_management_error)?,
        );
        return Ok(serde_json::json!({}));
    }
    let _ = request.reason;
    Err(unknown_task_error())
}

async fn subagent_output(agent: &ZedAgent, request: SubagentOutputRequest) -> Result<Value, SdkError> {
    let (_session, controller) = session_controller(agent, &request.session_id)?;
    let tail = normalize_output_tail_lines(request.tail).map_err(|error| {
        SdkError::invalid_params().data(serde_json::json!({
            "reason": "invalid_tail",
            "detail": error.to_string(),
        }))
    })?;
    let children = controller.status_entries().await;
    let output = if owned_child_ids(&request.session_id, &children).contains(request.task_id.as_str()) {
        let snapshot = controller
            .snapshot_for_thread(&request.task_id)
            .await
            .map_err(internal_management_error)?;
        let transcript = snapshot
            .snapshot
            .messages
            .iter()
            .map(|message| message.content.as_text())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        extract_tail_lines(&transcript, tail)
    } else {
        let owned_background = controller.background_status_entries().await.into_iter().any(|entry| {
            entry.id == request.task_id && entry.owner_session_id.as_deref() == Some(request.session_id.as_str())
        });
        if !owned_background {
            return Err(unknown_task_error());
        }
        controller
            .background_output_tail(&request.task_id, Some(tail))
            .await
            .map_err(internal_management_error)?
    };
    Ok(serde_json::json!({ "output": output }))
}

pub(super) fn owned_child_ids<'a>(session_id: &str, entries: &'a [SubagentStatusEntry]) -> HashSet<&'a str> {
    let mut owned_sessions = HashSet::from([session_id]);
    let mut owned_ids = HashSet::new();
    loop {
        let before = owned_ids.len();
        for entry in entries {
            if owned_sessions.contains(entry.parent_thread_id.as_str()) {
                let _ = owned_ids.insert(entry.id.as_str());
                let _ = owned_sessions.insert(entry.session_id.as_str());
            }
        }
        if owned_ids.len() == before {
            break;
        }
    }
    owned_ids
}

fn managed_child_task(entry: &SubagentStatusEntry) -> ManagedTask {
    let status = match entry.status {
        SubagentStatus::Queued | SubagentStatus::Running | SubagentStatus::Waiting => "running",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Closed => "killed",
    };
    ManagedTask {
        task_id: entry.id.clone(),
        description: entry.description.clone(),
        status,
        agent_id: Some(entry.session_id.clone()),
        subagent_type: Some(entry.agent_name.clone()),
        started_at_epoch_seconds: epoch_seconds(entry.created_at),
        ended_at_epoch_seconds: entry.completed_at.map(epoch_seconds),
        stop_reason: entry.error.clone(),
    }
}

fn managed_background_task(entry: &BackgroundSubprocessEntry) -> ManagedTask {
    let status = match entry.status {
        BackgroundSubprocessStatus::Starting | BackgroundSubprocessStatus::Running => "running",
        BackgroundSubprocessStatus::Stopped => "completed",
        BackgroundSubprocessStatus::Error => "failed",
    };
    ManagedTask {
        task_id: entry.id.clone(),
        description: entry.description.clone(),
        status,
        agent_id: Some(entry.session_id.clone()),
        subagent_type: Some(entry.agent_name.clone()),
        started_at_epoch_seconds: epoch_seconds(entry.started_at.unwrap_or(entry.created_at)),
        ended_at_epoch_seconds: entry.ended_at.map(epoch_seconds),
        stop_reason: entry.error.clone(),
    }
}

fn unknown_task_error() -> SdkError {
    SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_task" }))
}

fn internal_management_error(error: anyhow::Error) -> SdkError {
    SdkError::internal_error().data(serde_json::json!({
        "reason": "subagent_management_failed",
        "detail": error.to_string(),
    }))
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
