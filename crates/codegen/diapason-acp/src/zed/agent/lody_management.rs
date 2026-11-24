//! Enforces the caller-session-owned boundary for Lody subagent management RPCs.

use crate::acp;
use agent_client_protocol::Error as SdkError;
#[cfg(test)]
use diapason_core::subagents::BackgroundSubprocessStatus;
use diapason_core::subagents::{
    BackgroundLiveness, BackgroundOutputError, BackgroundSubprocessEntry, SubagentController, SubagentStatusEntry,
    TaskOwner, owned_background_ids, owned_child_ids, resolve_owned_background_id,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;

use super::super::types::SessionHandle;
use super::{ZedAgent, log_suppressed_error, task_lifecycle::bounded_session_correlation};

mod observability;
mod output_tail;
mod projection;

use self::observability::observe_management_request;
use self::output_tail::{foreground_output_tail, normalize_output_tail_lines};
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

/// The bounded detail reported for a stop whose exec-session close is owed.
///
/// The record's fault text names the exec session the close could not
/// confirm, so the client receives this category instead.
const CLOSE_PENDING_DETAIL: &str = "background_close_unconfirmed";

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
        // A stop that could not confirm the exec session closed is retryable
        // and distinct from an unexpected failure, so it keeps its own
        // category instead of collapsing into the generic one.
        Some("subagent_close_pending") => "subagent_close_pending",
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
/// This function resolves the collector and the span correlation, then
/// delegates to the observation boundary in [`observability`], which owns the
/// metrics guard, the `acp_lody_subagent_management` span and the bounded
/// failure log. Routing and parameter parsing stay in
/// `dispatch_management_request`, and the query helpers below stay pure. All
/// three operations are counted, including `list` and `output`, because the
/// counters describe use of the management surface rather than state mutation.
pub(super) async fn handle_lody_subagent_management(
    agent: &ZedAgent,
    method: &str,
    params: Value,
) -> Result<Value, SdkError> {
    let session_correlation = request_session_correlation(&params);
    let metrics = agent.local_tool_registry.metrics_collector();
    // The dispatch future holds the snapshot and output payloads for all three
    // operations, which is far too large to keep inline in every caller's
    // frame; boxing it here keeps the whole management surface cheap to call.
    let dispatch = Box::pin(dispatch_management_request(agent, method, params));
    observe_management_request(metrics.as_ref(), method, &session_correlation, dispatch).await
}

/// Routes a management method to its query or command helper.
///
/// The request payloads and the bounded error reasons belong to the helpers;
/// this function only parses parameters once and selects the method.
async fn dispatch_management_request(agent: &ZedAgent, method: &str, params: Value) -> Result<Value, SdkError> {
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

fn unknown_session_error() -> SdkError {
    SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_session" }))
}

/// Resolves a controller only while the session stays open.
///
/// A session that has begun closing releases its workspace runtime, so the
/// resolver falls back to the launch-scope controller. Serving the request
/// there would expose launch-scope tasks through a closed durable session, so a
/// closing session is reported exactly like an unknown one.
///
/// The check brackets `resolve` for that reason. A request can pass the first
/// check, then wait while the session closes and releases its runtime; the
/// resolution that follows can still return a controller. Checking again before
/// the result is interpreted rejects that interleaving, and the first check
/// keeps the reason stable whether or not a controller is still reachable. The
/// second check also outranks the resolver's own outcome, so a resolution that
/// fails, or finds nothing, because the session closed is still reported as
/// unknown rather than as an internal or unavailable error.
fn resolve_open_session_controller(
    session: &SessionHandle,
    resolve: impl FnOnce() -> Result<Option<Arc<SubagentController>>, SdkError>,
) -> Result<Arc<SubagentController>, SdkError> {
    if session.is_closing() {
        return Err(unknown_session_error());
    }
    // The resolver's outcome is captured before the second check so that a
    // close outranks it: a resolution that failed, or that found nothing,
    // because the session closed must still report `unknown_session` rather
    // than an internal or unavailable error the caller cannot act on.
    let resolved = resolve();
    if session.is_closing() {
        return Err(unknown_session_error());
    }
    let controller = resolved?.ok_or_else(|| {
        SdkError::invalid_params().data(serde_json::json!({ "reason": "subagent_management_unavailable" }))
    })?;
    Ok(controller)
}

fn session_controller(
    agent: &ZedAgent,
    session_id: &str,
) -> Result<(SessionHandle, Arc<SubagentController>), SdkError> {
    let session_id = acp::SessionId::new(session_id);
    let session = agent
        .session_handle(&session_id)
        .map_err(internal_management_error)?
        .ok_or_else(unknown_session_error)?;
    let controller = resolve_open_session_controller(&session, || {
        agent.session_subagent_controller(&session).map_err(internal_management_error)
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

async fn background_status_entries(
    controllers: &[Arc<SubagentController>],
    owner: TaskOwner<'_>,
) -> Vec<BackgroundSubprocessEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        entries.extend(controller.background_status_entries(owner).await);
    }
    entries
}

/// Projects background entries with liveness overlaid, without side effects.
///
/// `background_status_entries_with_liveness` observes the exec-session
/// manager read-only. Listing must not call `refresh_background_processes`:
/// that path mutates records, may respawn a process, publishes lifecycle
/// updates and persists state, so a query could restart work.
async fn observed_background_entries(
    controllers: &[Arc<SubagentController>],
    owner: TaskOwner<'_>,
) -> Vec<BackgroundSubprocessEntry> {
    let mut entries = Vec::new();
    for controller in controllers {
        for observed in controller.background_status_entries_with_liveness(owner).await {
            if let BackgroundLiveness::Unobserved { reason } = &observed.liveness {
                // The task keeps its cached status, but the operator needs to
                // know the liveness read failed. The task id and reason are
                // bounded; no exec-session handle, path or secret is logged.
                tracing::debug!(
                    task_id = observed.entry.id.as_str(),
                    reason = reason.as_str(),
                    "Background liveness read failed; keeping the cached status"
                );
            }
            entries.push(observed.entry);
        }
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

/// Finds the session controller tracking an owned background task and resolves
/// the requested id to its canonical owner-scoped form.
///
/// The controller's map and the listed projection are keyed by canonical ids.
/// A client may still hold the pre-owner-scoping id, so the request is resolved
/// through [`resolve_owned_background_id`] before the ownership comparison; the
/// resolved id is returned so the caller can address the record directly. The
/// fallback only matches records this session owns, so it never reaches another
/// session's task.
async fn controller_tracking_background(
    controllers: &[Arc<SubagentController>],
    session_id: &str,
    task_id: &str,
) -> Option<(Arc<SubagentController>, String)> {
    for controller in controllers {
        // A background record is owned by the session that launched it, so the
        // management surface presents that session's authority.
        let entries = background_status_entries(std::slice::from_ref(controller), TaskOwner::Session(session_id)).await;
        if let Some(resolved_task_id) = resolve_owned_background_id(session_id, &entries, task_id) {
            return Some((Arc::clone(controller), resolved_task_id.to_owned()));
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
    let backgrounds = observed_background_entries(&controllers, TaskOwner::Session(request.session_id.as_str())).await;
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
    let owned_children = owned_child_ids(&request.session_id, &children);
    if owned_children.contains(request.task_id.as_str()) {
        let controller = controller_tracking_child(&controllers, &request.task_id)
            .await
            .ok_or_else(unknown_task_error)?;
        // A grandchild lives in the child-scoped controller, which cannot prove
        // this session's ancestry from its own records, so the set resolved
        // over the whole tree above is the authority the command presents.
        drop(
            controller
                .close(TaskOwner::SessionTree(&owned_children), &request.task_id)
                .await
                .map_err(internal_management_error)?,
        );
        return Ok(serde_json::json!({}));
    }
    if let Some((controller, resolved_task_id)) =
        controller_tracking_background(&controllers, &request.session_id, &request.task_id).await
    {
        let outcome = controller
            .force_cancel_background(TaskOwner::Session(&request.session_id), &resolved_task_id)
            .await
            .map_err(internal_management_error)?;
        // A stop is complete only once the exec session is closed. Reporting
        // success for a close the runtime could not confirm told the client a
        // task had stopped while its process might still be running.
        if outcome.is_close_pending() {
            // The record's fault names the exec session the close could not
            // confirm, so only the bounded category is returned; the fault
            // itself stays in the debug-only suppressed detail.
            if let Some(fault) = outcome.entry().error.as_deref() {
                // The helper takes `&impl Display`; its implicit `Sized` bound
                // excludes `str`, so the fault slice is passed by reference.
                log_suppressed_error(&fault, "lody_subagent_management", CLOSE_PENDING_DETAIL);
            }
            return Err(SdkError::internal_error()
                .data(serde_json::json!({ "reason": "subagent_close_pending", "detail": CLOSE_PENDING_DETAIL })));
        }
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
            "detail": error,
        }))
    })?;
    let children = child_status_entries(&controllers).await;
    let owned_children = owned_child_ids(&request.session_id, &children);
    let output = if owned_children.contains(request.task_id.as_str()) {
        let controller = controller_tracking_child(&controllers, &request.task_id)
            .await
            .ok_or_else(unknown_task_error)?;
        let snapshot = controller
            .snapshot_for_thread(TaskOwner::SessionTree(&owned_children), &request.task_id)
            .await
            .map_err(internal_management_error)?;
        foreground_output_tail(&snapshot.snapshot.messages, tail)
    } else {
        let Some((controller, resolved_task_id)) =
            controller_tracking_background(&controllers, &request.session_id, &request.task_id).await
        else {
            return Err(unknown_task_error());
        };
        // The controller's own tail read is a pure query: it reports the live
        // session while it exists and falls back to the recorded transcript or
        // archive once it is gone. Refreshing here would turn a read into a
        // command that can respawn processes and fail on unrelated records.
        controller
            .background_output_tail(TaskOwner::Session(&request.session_id), &resolved_task_id, tail)
            .await
            .map_err(internal_management_error)?
    };
    Ok(serde_json::json!({ "output": output }))
}

fn unknown_task_error() -> SdkError {
    SdkError::invalid_params().data(serde_json::json!({ "reason": "unknown_task" }))
}

/// Reports an unexpected management failure as a bounded category.
///
/// A domain error can carry an exec-session identifier or a recorded archive
/// path in its own message or in a context frame, and the client did not ask
/// for either, so the payload names a closed category instead of the cause.
/// The categories are drawn from the failure the controller reports, keeping
/// the distinction between a live-exec-session read fault, an unreadable
/// transcript or archive, and any other failure. The full chain is logged at
/// `debug`, where ACP keeps suppressed detail.
fn internal_management_error(error: anyhow::Error) -> SdkError {
    let category = management_failure_detail(&error);
    log_suppressed_error(&error, "lody_subagent_management", category);
    SdkError::internal_error().data(serde_json::json!({
        "reason": "subagent_management_failed",
        "detail": category,
    }))
}

/// Classifies a management failure into one of its bounded detail categories.
///
/// A background output read reports its own [`BackgroundOutputError`], which
/// separates a live exec-session fault from an unreadable transcript or
/// archive. Anything else is an unexpected management failure, because the
/// remaining errors a management request can hit are local lock or
/// configuration faults rather than storage reads.
fn management_failure_detail(error: &anyhow::Error) -> &'static str {
    match error.downcast_ref::<BackgroundOutputError>() {
        Some(BackgroundOutputError::Live(_)) => "background_output_unavailable",
        Some(BackgroundOutputError::Archived(_)) => "background_output_unreadable",
        None => MANAGEMENT_FAILURE_CATEGORY,
    }
}

#[cfg(test)]
mod bounded_failure_tests {
    //! A management failure reports a bounded category, never its cause chain.
    use super::*;

    /// Extracts the client-visible `detail` of a mapped management failure.
    fn detail_of(error: SdkError) -> String {
        let data = error.data.expect("management failure data");
        assert_eq!(
            data.get("reason").and_then(Value::as_str),
            Some("subagent_management_failed"),
            "the bounded reason code must stay unchanged"
        );
        data.get("detail")
            .and_then(Value::as_str)
            .expect("bounded management detail")
            .to_string()
    }

    /// An unreadable transcript or archive must not republish its path.
    #[test]
    fn archived_output_failure_reports_a_bounded_category_without_its_path() {
        let cause = anyhow::anyhow!("failed to read /home/operator/.diapason/state/archive.jsonl");
        let error = anyhow::Error::new(BackgroundOutputError::Archived(cause))
            .context("read background output for background-worker@session-a");
        let detail = detail_of(internal_management_error(error));
        assert_eq!(detail, "background_output_unreadable", "the archive read failure keeps its category");
        assert!(!detail.contains("archive.jsonl"), "the detail must not carry the recorded archive path: {detail}");
        assert!(!detail.contains("session-a"), "the detail must not carry the task target: {detail}");
    }

    /// A live exec-session fault must not republish the session identifier.
    #[test]
    fn live_output_failure_reports_its_own_bounded_category() {
        let cause = anyhow::anyhow!("PTY session 'exec-worker-1' closed while reading");
        let error = anyhow::Error::new(BackgroundOutputError::Live(cause));
        let detail = detail_of(internal_management_error(error));
        assert_eq!(detail, "background_output_unavailable", "the live read failure keeps its own category");
        assert!(!detail.contains("exec-worker-1"), "the detail must not carry the exec-session id: {detail}");
    }

    /// Any other management failure keeps the generic bounded category.
    #[test]
    fn other_management_failures_report_the_generic_category() {
        let error = anyhow::anyhow!("failed to close /home/operator/.diapason/state/ledger.json");
        let detail = detail_of(internal_management_error(error));
        assert_eq!(detail, MANAGEMENT_FAILURE_CATEGORY, "an unclassified cause stays bounded");
    }

    /// A close the runtime could not confirm keeps its own bounded detail.
    #[test]
    fn close_pending_detail_stays_bounded() {
        assert!(
            !CLOSE_PENDING_DETAIL.contains("exec-"),
            "the close-pending detail must not name the exec session it could not close"
        );
        assert!(
            !CLOSE_PENDING_DETAIL.contains('/'),
            "the close-pending detail must not name a path: {CLOSE_PENDING_DETAIL}"
        );
    }
}

#[cfg(test)]
#[path = "lody_management_doc_tests.rs"]
mod doc_tests;
#[cfg(test)]
#[path = "lody_management_tests.rs"]
mod tests;
