//! Enforces the caller-session-owned boundary for Lody subagent management RPCs.

use crate::acp;
use agent_client_protocol::Error as SdkError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentController, SubagentStatus, SubagentStatusEntry,
    extract_tail_lines, normalize_output_tail_lines,
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
) -> Result<(SessionHandle, Arc<SubagentController>), SdkError> {
    let session_id = acp::SessionId::new(session_id);
    let session = agent
        .session_handle(&session_id)
        .map_err(internal_management_error)?
        .ok_or_else(|| SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_session" })))?;
    let controller = agent
        .session_subagent_controller(&session)
        .map_err(internal_management_error)?
        .ok_or_else(|| {
            SdkError::invalid_params().data(serde_json::json!({ "reason": "subagent_management_unavailable" }))
        })?;
    Ok((session, controller))
}

/// Returns the session's root controller followed by every child-scoped
/// controller reachable from it.
///
/// Grandchildren live in the child controller that spawned them, not in the
/// root controller's `children` map, so management operations must walk the
/// whole owned tree. The tree is reachable only through records this session
/// already owns, which preserves the root-session authorization boundary.
async fn session_controllers(
    agent: &ZedAgent,
    session_id: &str,
) -> Result<(SessionHandle, Vec<Arc<SubagentController>>), SdkError> {
    let (session, root) = session_controller(agent, session_id)?;
    let mut controllers = vec![Arc::clone(&root)];
    controllers.extend(root.descendant_controllers().await);
    Ok((session, controllers))
}

async fn child_status_entries(controllers: &[Arc<SubagentController>]) -> Vec<SubagentStatusEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(controller.status_entries().await);
    }
    entries
}

async fn background_status_entries(controllers: &[Arc<SubagentController>]) -> Vec<BackgroundSubprocessEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(controller.background_status_entries().await);
    }
    entries
}

async fn controller_tracking_child(
    controllers: &[Arc<SubagentController>],
    task_id: &str,
) -> Option<Arc<SubagentController>> {
    for controller in controllers {
        if controller.status_entries().await.iter().any(|entry| entry.id == task_id) {
            return Some(Arc::clone(controller));
        }
    }
    None
}

async fn controller_tracking_background(
    controllers: &[Arc<SubagentController>],
    session_id: &str,
    task_id: &str,
) -> Option<Arc<SubagentController>> {
    for controller in controllers {
        let owned = controller
            .background_status_entries()
            .await
            .into_iter()
            .any(|entry| entry.id == task_id && entry.owner_session_id.as_deref() == Some(session_id));
        if owned {
            return Some(Arc::clone(controller));
        }
    }
    None
}

async fn list_subagents(agent: &ZedAgent, request: ListSubagentsRequest) -> Result<Value, SdkError> {
    let (_session, controllers) = session_controllers(agent, &request.session_id).await?;
    let children = child_status_entries(&controllers).await;
    let owned_children = owned_child_ids(&request.session_id, &children);
    let mut tasks = children
        .iter()
        .filter(|entry| owned_children.contains(entry.id.as_str()))
        .filter(|entry| !request.active_only || !entry.status.is_terminal())
        .map(managed_child_task)
        .collect::<Vec<_>>();
    tasks.extend(
        background_status_entries(&controllers)
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
    let (_session, controllers) = session_controllers(agent, &request.session_id).await?;
    let children = child_status_entries(&controllers).await;
    if owned_child_ids(&request.session_id, &children).contains(request.task_id.as_str()) {
        let controller = controller_tracking_child(&controllers, &request.task_id)
            .await
            .ok_or_else(unknown_task_error)?;
        drop(controller.close(&request.task_id).await.map_err(internal_management_error)?);
        return Ok(serde_json::json!({}));
    }
    if let Some(controller) = controller_tracking_background(&controllers, &request.session_id, &request.task_id).await
    {
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
    let (_session, controllers) = session_controllers(agent, &request.session_id).await?;
    let tail = normalize_output_tail_lines(request.tail).map_err(|error| {
        SdkError::invalid_params().data(serde_json::json!({
            "reason": "invalid_tail",
            "detail": error.to_string(),
        }))
    })?;
    let children = child_status_entries(&controllers).await;
    let output = if owned_child_ids(&request.session_id, &children).contains(request.task_id.as_str()) {
        let controller = controller_tracking_child(&controllers, &request.task_id)
            .await
            .ok_or_else(unknown_task_error)?;
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
        let Some(controller) =
            controller_tracking_background(&controllers, &request.session_id, &request.task_id).await
        else {
            return Err(unknown_task_error());
        };
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
