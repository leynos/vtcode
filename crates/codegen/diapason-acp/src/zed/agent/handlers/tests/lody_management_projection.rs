//! Lifecycle publication and task-list projection for the Lody management
//! extensions.
use super::lody_support::{expected_epoch_seconds, lifecycle_task_meta, lody_list};
use super::*;
use diapason_core::subagents::{BackgroundSubprocessStatus, TaskOwner};
use std::time::Duration;

/// Attaches the lifecycle forwarder after a task is already running.
///
/// The attach must publish the task's current snapshot; otherwise a resumed
/// task that never transitions again stays invisible to the client.
pub(super) async fn publish_forwarder_snapshot_on_attach(
    agent: &ZedAgent,
    session: &SessionHandle,
    task_id: &str,
    notifications: &mut mpsc::UnboundedReceiver<acp::AgentNotification>,
) {
    agent
        .ensure_task_lifecycle_forwarder(session)
        .expect("start lifecycle forwarder for the attached session");
    let initial_task = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let notification = notifications.recv().await.expect("lifecycle notification stream");
            if let Some(task) = lifecycle_task_meta(&notification) {
                break task;
            }
        }
    })
    .await
    .expect("initial lifecycle snapshot deadline");
    assert_eq!(
        initial_task["taskId"], task_id,
        "a task that was already running must be published when the forwarder attaches"
    );
    assert_eq!(initial_task["kind"], "background");
    let restored_status = initial_task["status"].as_str().expect("projected task status");
    assert!(
        matches!(restored_status, "pending" | "in_progress"),
        "a restored task that is still running must be published as active, got {restored_status}"
    );
}

/// Asserts the list projections of an active background task.
///
/// Waits for the process to record its start, then returns the listed
/// `startedAtEpochSeconds` value so a later terminal projection can prove the
/// start instant did not move.
pub(super) async fn assert_active_task_projection(
    cx: &ConnectionTo<Agent>,
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> serde_json::Value {
    // The background process records its start asynchronously, so wait until
    // the controller observes it before asserting the projection.
    let active_entry = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let entry = controller
                .refresh_then_background_snapshot(TaskOwner::Workspace, task_id)
                .await
                .expect("read the owned background snapshot")
                .entry;
            if entry.started_at.is_some() {
                break entry;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the background task must record its start");

    let response = lody_list(cx, session_id, false).await.expect("list the owned tasks");
    assert_eq!(response["tasks"][0]["taskId"], task_id);
    let expected_status = match active_entry.status {
        BackgroundSubprocessStatus::Starting | BackgroundSubprocessStatus::Running => "running",
        BackgroundSubprocessStatus::Stopped => "completed",
        BackgroundSubprocessStatus::Error => "failed",
    };
    assert_eq!(
        response["tasks"][0]["status"], expected_status,
        "the listed status must project the controller record: {response}"
    );
    assert_eq!(
        response["tasks"][0]["startedAtEpochSeconds"],
        expected_epoch_seconds(active_entry.started_at.unwrap_or(active_entry.created_at)),
        "the listed start must project the controller record: {response}"
    );
    assert!(
        response["tasks"][0]["endedAtEpochSeconds"].is_null(),
        "an active task must not report an end instant: {response}"
    );
    let active_started_at_epoch_seconds = response["tasks"][0]["startedAtEpochSeconds"].clone();
    let active_response = lody_list(cx, session_id, true).await.expect("list the active owned tasks");
    assert_eq!(active_response["tasks"][0]["taskId"], task_id, "activeOnly must still list a task that is running");
    let active_status = active_response["tasks"][0]["status"].as_str().expect("projected task status");
    assert!(
        matches!(active_status, "pending" | "running"),
        "an active task must be reported as active, got {active_status}"
    );
    active_started_at_epoch_seconds
}
