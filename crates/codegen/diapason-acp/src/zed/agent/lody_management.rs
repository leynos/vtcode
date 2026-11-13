//! Enforces the caller-session-owned boundary for Lody subagent management RPCs.

use crate::acp;
use agent_client_protocol::Error as SdkError;
#[cfg(test)]
use diapason_core::subagents::BackgroundSubprocessStatus;
use diapason_core::subagents::{
    BackgroundSubprocessEntry, SubagentController, SubagentStatusEntry, extract_tail_lines,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::time::Instant;
use tracing::{Instrument, field, info_span, warn};

use super::super::types::SessionHandle;
use super::{
    ZedAgent,
    lody::{owned_background_ids, owned_child_ids},
    task_lifecycle::bounded_session_correlation,
};

mod projection;

use self::projection::{managed_background_task, managed_child_task};

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

pub(super) fn is_lody_subagent_management_method(method: &str) -> bool {
    matches!(
        method,
        super::lody::LODY_SUBAGENTS_LIST_METHOD
            | super::lody::LODY_SUBAGENTS_CANCEL_METHOD
            | super::lody::LODY_SUBAGENTS_OUTPUT_METHOD
    )
}

/// Serves one Lody management RPC at the adapter's dispatch boundary.
///
/// This is the only place a management request is observed: the metrics guard,
/// the `acp_lody_subagent_management` span and the bounded failure log all live
/// here, so the query helpers below stay pure. All three operations are
/// counted, including `list` and `output`, because the counters describe use of
/// the management surface rather than state mutation.
pub(super) async fn handle_lody_subagent_management(
    agent: &ZedAgent,
    method: &str,
    params: Value,
) -> Result<Value, SdkError> {
    let operation = ManagementOperation::of(method);
    let session_correlation = request_session_correlation(&params);
    let span = info_span!(
        "acp_lody_subagent_management",
        operation = operation.as_str(),
        session_correlation = %session_correlation,
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        error_category = field::Empty
    );
    // The guard owns the in-flight gauge, so a request dropped while it awaits
    // is still recorded as a failure instead of leaving the gauge elevated.
    let management = agent.local_tool_registry.metrics_collector().begin_acp_management();
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
        // The span has already exited by the time the failure is logged, so
        // the bounded correlation is repeated here rather than inherited.
        warn!(
            operation = operation.as_str(),
            session_correlation = %session_correlation,
            error_category,
            elapsed_ms,
            "Lody subagent management request failed"
        );
    }
    management.finish(result.is_ok());
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

/// Projects background entries with liveness overlaid, without side effects.
///
/// `background_status_entries_with_liveness` observes the exec-session
/// manager read-only. Listing must not call `refresh_background_processes`:
/// that path mutates records, may respawn a process, publishes lifecycle
/// updates and persists state, so a query could restart work.
async fn observed_background_entries(controllers: &[Arc<SubagentController>]) -> Vec<BackgroundSubprocessEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(controller.background_status_entries_with_liveness().await);
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
    let backgrounds = observed_background_entries(&controllers).await;
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

/// Default number of output lines returned when a client omits `tail`.
///
/// This is a transport-contract default: it belongs to the ACP adapter, not to
/// the domain controller that only applies a caller-supplied bound.
const DEFAULT_TASK_OUTPUT_TAIL_LINES: usize = 200;

/// Hard upper bound on a client-requested output tail.
///
/// The bound keeps a single ACP response bounded; the domain controller has no
/// opinion about it.
const MAX_TASK_OUTPUT_TAIL_LINES: usize = 10_000;

/// Resolves a requested output-tail size against the ACP transport contract.
///
/// `None` selects [`DEFAULT_TASK_OUTPUT_TAIL_LINES`]; values above
/// [`MAX_TASK_OUTPUT_TAIL_LINES`] are rejected so one response cannot grow
/// without bound.
fn normalize_output_tail_lines(max_lines: Option<usize>) -> Result<usize, String> {
    let max_lines = max_lines.unwrap_or(DEFAULT_TASK_OUTPUT_TAIL_LINES);
    if max_lines > MAX_TASK_OUTPUT_TAIL_LINES {
        return Err(format!("output tail exceeds the hard maximum of {MAX_TASK_OUTPUT_TAIL_LINES} lines"));
    }
    Ok(max_lines)
}

async fn subagent_output(agent: &ZedAgent, request: SubagentOutputRequest) -> Result<Value, SdkError> {
    let (_session, controllers) = session_controllers(agent, &request.session_id).await?;
    let tail = normalize_output_tail_lines(request.tail).map_err(|error| {
        SdkError::invalid_params().data(serde_json::json!({
            "reason": "invalid_tail",
            "detail": error,
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
        // The controller's own tail read is a pure query: it reports the live
        // session while it exists and falls back to the recorded transcript or
        // archive once it is gone. Refreshing here would turn a read into a
        // command that can respawn processes and fail on unrelated records.
        controller
            .background_output_tail(&request.task_id, tail)
            .await
            .map_err(internal_management_error)?
    };
    Ok(serde_json::json!({ "output": output }))
}

fn unknown_task_error() -> SdkError {
    SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_task" }))
}

/// Maps a management failure to an ACP error, keeping its whole cause chain.
///
/// Domain errors expose their cause through [`std::error::Error::source`]
/// rather than in their own `Display`, so the alternate selector is required
/// for the client to see why a request failed.
fn internal_management_error(error: anyhow::Error) -> SdkError {
    SdkError::internal_error().data(serde_json::json!({
        "reason": "subagent_management_failed",
        "detail": format!("{error:#}"),
    }))
}

#[cfg(test)]
#[path = "lody_management_doc_tests.rs"]
mod doc_tests;
#[cfg(test)]
#[path = "lody_management_tests.rs"]
mod tests;
