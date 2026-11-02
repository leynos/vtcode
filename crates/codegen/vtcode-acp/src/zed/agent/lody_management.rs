//! Enforces the caller-session-owned boundary for Lody subagent management RPCs.

use crate::acp;
use agent_client_protocol::Error as SdkError;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tracing::{Instrument, field, info_span, warn};
use vtcode_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentController, SubagentStatus, SubagentStatusEntry,
    extract_tail_lines, normalize_output_tail_lines,
};

use super::super::types::SessionHandle;
use super::{
    ZedAgent,
    lody::{owned_background_ids, owned_child_ids},
    task_lifecycle::bounded_session_correlation,
};

/// The bounded set of Lody subagent management RPCs.
///
/// The identity is deliberately a closed enum: it becomes a span field and
/// metrics dimension, so a client-supplied method name must never reach either.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ManagementOperation {
    List,
    Cancel,
    Output,
}

impl ManagementOperation {
    fn of(method: &str) -> Self {
        match method {
            super::lody::LODY_SUBAGENTS_CANCEL_METHOD => Self::Cancel,
            super::lody::LODY_SUBAGENTS_OUTPUT_METHOD => Self::Output,
            _ => Self::List,
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Cancel => "cancel",
            Self::Output => "output",
        }
    }
}

/// Bounded failure categories recorded on the management span.
const MANAGEMENT_FAILURE_CATEGORY: &str = "management_failed";

/// Correlates a management request with its session without logging the ID.
fn request_session_correlation(params: &Value) -> String {
    params
        .get("sessionId")
        .and_then(Value::as_str)
        .map(|session_id| bounded_session_correlation(&acp::SessionId::new(Arc::from(session_id))))
        .unwrap_or_else(|| "unknown".to_string())
}

/// Maps a management error to one of the extension's bounded reason strings.
fn management_error_reason(error: &SdkError) -> &'static str {
    let reason = error.data.as_ref().and_then(|data| data.get("reason")).and_then(Value::as_str);
    match reason {
        Some("invalid_lody_subagent_params") => "invalid_lody_subagent_params",
        Some("unknown_session") => "unknown_session",
        Some("subagent_management_unavailable") => "subagent_management_unavailable",
        Some("unknown_task") => "unknown_task",
        Some("invalid_tail") => "invalid_tail",
        Some("subagent_management_failed") => "subagent_management_failed",
        _ => MANAGEMENT_FAILURE_CATEGORY,
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
    let operation = ManagementOperation::of(method);
    let span = info_span!(
        "acp_lody_subagent_management",
        operation = operation.as_str(),
        session_correlation = %request_session_correlation(&params),
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        error_category = field::Empty
    );
    let metrics = agent.local_tool_registry.metrics_collector();
    metrics.record_acp_management_started();
    let started_at = Instant::now();
    let result = async {
        match method {
            super::lody::LODY_SUBAGENTS_LIST_METHOD => list_subagents(agent, parse_params(params)?).await,
            super::lody::LODY_SUBAGENTS_CANCEL_METHOD => cancel_subagent(agent, parse_params(params)?).await,
            super::lody::LODY_SUBAGENTS_OUTPUT_METHOD => subagent_output(agent, parse_params(params)?).await,
            _ => Err(SdkError::method_not_found()),
        }
    }
    .instrument(span.clone())
    .await;
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _outcome_recorded = span.record("outcome", if result.is_ok() { "succeeded" } else { "failed" });
    let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_ms);
    if let Err(error) = &result {
        let error_category = management_error_reason(error);
        let _error_category_recorded = span.record("error_category", error_category);
        warn!(
            operation = operation.as_str(),
            error_category, elapsed_ms, "Lody subagent management request failed"
        );
    }
    metrics.record_acp_management_finished(result.is_ok(), elapsed_ms);
    result
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

/// Collects background entries after refreshing each controller from the
/// exec-session manager.
///
/// `background_status_entries` reads the cached projection, and exit
/// detection exists only in `refresh_background_processes`. Listing without a
/// refresh would therefore keep reporting a process that already exited as
/// running until an unrelated cancel or output request refreshed it.
async fn refreshed_background_entries(
    controllers: &[Arc<SubagentController>],
) -> Result<Vec<BackgroundSubprocessEntry>, SdkError> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(
            controller
                .refresh_background_processes()
                .await
                .map_err(internal_management_error)?,
        );
    }
    Ok(entries)
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
        let entries = controller.background_status_entries().await;
        if owned_background_ids(session_id, &entries).contains(task_id) {
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
    let backgrounds = refreshed_background_entries(&controllers).await?;
    let owned_backgrounds = owned_background_ids(&request.session_id, &backgrounds);
    tasks.extend(
        backgrounds
            .iter()
            .filter(|entry| owned_backgrounds.contains(entry.id.as_str()))
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
        // Refreshing is a command with side effects (exit detection, lifecycle
        // publication, persistence), so it stays an explicit step here while
        // the output read remains a pure query.
        drop(
            controller
                .refresh_background_processes()
                .await
                .map_err(internal_management_error)?,
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use vtcode_config::VTCodeConfig;
    use vtcode_core::config::types::ReasoningEffortLevel;
    use vtcode_core::subagents::SubagentControllerConfig;
    use vtcode_core::tools::exec_session::ExecSessionManager;
    use vtcode_core::tools::registry::PtySessionManager;

    fn persisted_running_record_json() -> Value {
        serde_json::json!({
            "records": [{
                "id": "background-worker",
                "agent_name": "worker",
                "display_label": "worker",
                "description": "worker",
                "source": "builtin",
                "color": null,
                "owner_session_id": "session-a",
                "session_id": "session-worker",
                "exec_session_id": "exec-missing",
                "desired_enabled": false,
                "status": "running",
                "created_at": "2026-08-28T00:00:00Z",
                "updated_at": "2026-08-28T00:00:00Z",
                "started_at": "2026-08-28T00:00:00Z",
                "ended_at": null,
                "pid": 4321,
                "prompt": "work",
                "summary": null,
                "error": null,
                "archive_path": null,
                "transcript_path": null,
                "max_turns": null,
                "model_override": null,
                "reasoning_override": null,
                "restart_attempts": 0
            }]
        })
    }

    /// `_lody/subagents/list` projects the cached record set, and exit
    /// detection lives only in `refresh_background_processes`. Without the
    /// refresh, a process that already exited is still listed as running.
    #[tokio::test]
    async fn listing_refreshes_background_records_before_projection() {
        let temp = TempDir::new().expect("tempdir");
        let state_dir = temp.path().join(".vtcode/state");
        std::fs::create_dir_all(&state_dir).expect("state dir");
        std::fs::write(state_dir.join("background_subagents.json"), persisted_running_record_json().to_string())
            .expect("write state");

        let vt_cfg = VTCodeConfig::default();
        let pty_sessions = PtySessionManager::new(temp.path().to_path_buf(), vt_cfg.pty.clone());
        let exec_sessions = ExecSessionManager::new(temp.path().to_path_buf(), pty_sessions.clone());
        let controller = Arc::new(
            SubagentController::new_with_background_owner(
                SubagentControllerConfig {
                    workspace_root: temp.path().to_path_buf(),
                    parent_session_id: "session-a".to_string(),
                    parent_model: "test-model".to_string(),
                    parent_provider: "openai".to_string(),
                    parent_reasoning_effort: ReasoningEffortLevel::Medium,
                    api_key: "test-key".to_string(),
                    vt_cfg,
                    openai_chatgpt_auth: None,
                    depth: 0,
                    workspace_gated: false,
                    exec_sessions,
                    pty_manager: pty_sessions.manager().clone(),
                    managed_background_runtime: false,
                },
                Some("session-a"),
            )
            .await
            .expect("scoped controller"),
        );
        let controllers = [Arc::clone(&controller)];

        let cached = background_status_entries(&controllers).await;
        assert_eq!(
            cached.iter().map(|entry| entry.status).collect::<Vec<_>>(),
            [BackgroundSubprocessStatus::Running],
            "the cached projection is what the list path used before the refresh"
        );

        let refreshed = refreshed_background_entries(&controllers)
            .await
            .expect("refresh background entries");
        assert_eq!(
            refreshed.iter().map(|entry| entry.status).collect::<Vec<_>>(),
            [BackgroundSubprocessStatus::Stopped],
            "listing must observe the exit instead of reporting a stale running process"
        );
    }
}
