//! Lifecycle publication and task-list projection for the Lody management
//! extensions.
use super::lody_support::{expected_epoch_seconds, lifecycle_announces_task, lifecycle_task_meta, lody_list};
use super::*;
use diapason_core::subagents::{BackgroundSubprocessStatus, SubagentController, TaskOwner};
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
            let Some(task) = lifecycle_task_meta(&notification) else {
                continue;
            };
            if task["taskId"] != task_id {
                continue;
            }
            assert!(
                lifecycle_announces_task(&notification),
                "the first snapshot for an attached task must be its initial ToolCall, not an update for a \
                 call id the client never saw"
            );
            break task;
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

/// Asserts the owning session lists exactly its own delegation tree.
///
/// The root controller tracks the `default` child; that child's scoped
/// controller tracks both `explorer` grandchildren. Every listed task must be
/// one of those three ids, and each projection must agree with the record the
/// owning controller holds.
pub(super) async fn assert_owned_tree_is_listed(
    cx: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    root: &Arc<SubagentController>,
    child_controller: &Arc<SubagentController>,
    child_id: &str,
    grandchild_ids: [&str; 2],
) -> Result<(), acp::Error> {
    let listed = lody_list(cx, session_id, false).await?;
    let tasks = listed["tasks"].as_array().expect("Lody task list").clone();
    // Every listed row must carry a string id, so a malformed row cannot be
    // dropped by the projection below and let the comparison pass anyway.
    let mut listed_ids = tasks
        .iter()
        .map(|task| task["taskId"].as_str().expect("every listed task must carry a string task ID"))
        .collect::<Vec<_>>();
    listed_ids.sort_unstable();
    let mut expected_ids = vec![child_id, grandchild_ids[0], grandchild_ids[1]];
    expected_ids.sort_unstable();
    assert_eq!(
        listed_ids, expected_ids,
        "the root session must list its child and both grandchildren exactly once: {listed}"
    );
    let root_entries = root.status_entries().await;
    let child_entries = child_controller.status_entries().await;
    for task in &tasks {
        let task_id = task["taskId"].as_str().expect("listed task ID");
        let record = if task_id == child_id {
            root_entries.iter().find(|entry| entry.id == task_id)
        } else {
            child_entries.iter().find(|entry| entry.id == task_id)
        }
        .expect("the owning controller must track every listed task");
        assert_eq!(task["status"], "running", "a live foreground task must be listed as running: {task}");
        assert_eq!(
            task["startedAtEpochSeconds"],
            expected_epoch_seconds(record.created_at),
            "the listed start must project the owning controller record: {task}"
        );
        assert!(
            task["endedAtEpochSeconds"].is_null(),
            "an active foreground task must not report an end instant: {task}"
        );
    }
    let active_listed = lody_list(cx, session_id, true).await?;
    assert_eq!(active_listed, listed, "every spawned task is still running, so activeOnly must change nothing");
    Ok(())
}

/// Asserts the list projections of an active background task.
///
/// Waits for the process to record its start, then returns the listed
/// `startedAtEpochSeconds` value so a later terminal projection can prove the
/// start instant did not move.
pub(super) async fn assert_active_task_projection(
    cx: &ConnectionTo<Agent>,
    controller: &Arc<SubagentController>,
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
