//! Enforces the caller-session-owned boundary for Lody subagent management RPCs.

use crate::acp;
use agent_client_protocol::Error as SdkError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentStatus, SubagentStatusEntry, extract_tail_lines,
    normalize_output_tail_lines,
};

use super::super::types::SessionHandle;
use super::{ZedAgent, lody::owned_child_ids};

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

pub(super) fn is_lody_subagent_management_method(method: &str) -> bool {
    matches!(
        method,
        super::lody::LODY_SUBAGENTS_LIST_METHOD
            | super::lody::LODY_SUBAGENTS_CANCEL_METHOD
            | super::lody::LODY_SUBAGENTS_OUTPUT_METHOD
    )
}

pub(super) async fn handle_lody_subagent_management(
    agent: &ZedAgent,
    method: &str,
    params: Value,
) -> Result<Value, SdkError> {
    match method {
        super::lody::LODY_SUBAGENTS_LIST_METHOD => list_subagents(agent, parse_params(params)?).await,
        super::lody::LODY_SUBAGENTS_CANCEL_METHOD => cancel_subagent(agent, parse_params(params)?).await,
        super::lody::LODY_SUBAGENTS_OUTPUT_METHOD => subagent_output(agent, parse_params(params)?).await,
        _ => Err(SdkError::method_not_found()),
    }
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
) -> Result<(SessionHandle, Arc<vtcode_core::subagents::SubagentController>), SdkError> {
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

fn epoch_seconds(timestamp: chrono::DateTime<chrono::Utc>) -> f64 {
    timestamp.timestamp_millis().max(0) as f64 / 1_000.0
}
