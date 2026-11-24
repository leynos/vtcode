//! Read-boundary regressions for the Lody management extensions.
//!
//! Every management read is probed against the controller's observable state:
//! records, timestamps, persisted ledger bytes and process liveness are sampled
//! immediately before and after the request. The probe itself never mutates, so
//! a difference between the samples can only come from the request under test.
use super::lody_support::{
    LODY_EXITING_BACKGROUND_SCRIPT, RecordingBackgroundObserver, drain_notifications, lody_output_tail,
    observable_background_snapshot, write_lody_background_script,
};
use super::*;
use diapason_core::subagents::{BackgroundSubprocessEntry, SpawnBackgroundSubprocessRequest, TaskOwner};
use std::path::Path;
use std::time::Duration;

/// A background record whose process has exited while the controller still
/// caches the record as active, plus the output that process recorded.
pub(super) struct StaleBackgroundRecord {
    entry: BackgroundSubprocessEntry,
    recorded_output: String,
}

impl StaleBackgroundRecord {
    /// Record id of the stale launch.
    pub(super) fn id(&self) -> &str {
        &self.entry.id
    }
}

/// Settles the lifecycle notification stream before a read probe.
///
/// A delivered notification restarts the quiet window and the outer deadline
/// ends it, so a steady stream cannot hang the test.
pub(super) async fn settle_lifecycle_notifications(
    notifications: &mut mpsc::UnboundedReceiver<acp::AgentNotification>,
) {
    drop(
        tokio::time::timeout(Duration::from_secs(2), async {
            while tokio::time::timeout(Duration::from_millis(100), notifications.recv())
                .await
                .is_ok()
            {}
        })
        .await,
    );
}

/// Reads a live task's output until the fixture records its final line.
///
/// The wait is bounded by a deadline that fails with a clear message. Every
/// iteration also proves the read neither changed the controller's observable
/// state nor published a status update.
pub(super) async fn read_live_output_until_recorded(
    cx: &ConnectionTo<Agent>,
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    task_id: &str,
    workspace_path: &Path,
    notifications: &mut mpsc::UnboundedReceiver<acp::AgentNotification>,
) -> Result<serde_json::Value, acp::Error> {
    // The spawn publishes its run-started transition asynchronously. Settle
    // that traffic before the probe window so the comparison below attributes
    // every difference to the read under test.
    settle_lifecycle_notifications(notifications).await;
    // The lifecycle forwarder delivers asynchronously, so draining the client
    // channel after the response would race its delivery. The controller calls
    // its progress observers synchronously from the publication site, so a
    // recording observer proves the read published nothing without depending
    // on delivery timing.
    let observer = Arc::new(RecordingBackgroundObserver::default());
    let _observer_registration = controller.register_progress_observer(
        session_id.0.to_string(),
        Arc::clone(&observer) as Arc<dyn diapason_core::subagents::SubagentProgressObserver>,
    );
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let _ = observer.take();
            let before = observable_background_snapshot(
                controller,
                TaskOwner::Session(session_id.0.as_ref()),
                workspace_path,
                0,
            )
            .await;
            let output = lody_output_tail(cx, session_id, task_id, 1).await?;
            let published = observer.take();
            let after = observable_background_snapshot(
                controller,
                TaskOwner::Session(session_id.0.as_ref()),
                workspace_path,
                0,
            )
            .await;
            assert_eq!(
                after, before,
                "an output read must not change records, timestamps, ledger bytes, or process state"
            );
            assert!(published.is_empty(), "an output read must not publish a background status update: {published:?}");
            if output["output"] == "lody-output-two" {
                break Ok::<serde_json::Value, acp::Error>(output);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the live background task must record its final output before the deadline")
}

/// Reads an exited task's output, proving the archive fallback is inert.
///
/// A read of a task whose process has already exited must be as inert as a live
/// read: the exec session is gone, so the reader falls back to the archive; the
/// request must not settle, respawn, persist, or publish anything.
pub(super) async fn read_exited_output_is_inert(
    cx: &ConnectionTo<Agent>,
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    task_id: &str,
    workspace_path: &Path,
    notifications: &mut mpsc::UnboundedReceiver<acp::AgentNotification>,
) -> Result<serde_json::Value, acp::Error> {
    // Cancelling the task published its terminal transition asynchronously.
    // Settle that traffic before the probe window so the comparison below
    // attributes every difference to the read under test.
    settle_lifecycle_notifications(notifications).await;
    let exited_observer = Arc::new(RecordingBackgroundObserver::default());
    let _exited_observer_registration = controller.register_progress_observer(
        session_id.0.to_string(),
        Arc::clone(&exited_observer) as Arc<dyn diapason_core::subagents::SubagentProgressObserver>,
    );
    let _ = exited_observer.take();
    let before =
        observable_background_snapshot(controller, TaskOwner::Session(session_id.0.as_ref()), workspace_path, 0).await;
    let exited_output = lody_output_tail(cx, session_id, task_id, 20).await?;
    let published = exited_observer.take();
    let after =
        observable_background_snapshot(controller, TaskOwner::Session(session_id.0.as_ref()), workspace_path, 0).await;
    assert_eq!(
        after, before,
        "an output read of an exited task must not change records, timestamps, ledger bytes, or process state"
    );
    assert!(
        published.is_empty(),
        "an output read of an exited task must not publish a background status update: {published:?}"
    );
    assert!(
        exited_output["output"].as_str().is_some(),
        "an exited task must still answer with a bounded output string: {exited_output}"
    );
    Ok(exited_output)
}

/// Spawns a task whose child exits after recording its output, and returns the
/// record the controller still caches as active.
///
/// A record the controller still caches as active, whose child has already
/// exited, is the case an accidental refresh would settle. The shared fixture
/// script is rewritten to exit after its output, so the task's process ends on
/// its own while the controller still caches the record as active.
pub(super) async fn spawn_stale_background_record(
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    workspace_path: &Path,
) -> StaleBackgroundRecord {
    write_lody_background_script(workspace_path, LODY_EXITING_BACKGROUND_SCRIPT);
    let stale = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect("spawn a second owned background task");
    let recorded_output = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let output = controller
                .background_output_tail(TaskOwner::Workspace, &stale.id, 20)
                .await
                .expect("read the second task's live output");
            if output.contains("lody-output-two") {
                break output;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the second background task must record its output");
    // The liveness projection reports the ended exec session without mutating
    // the cached record, so it is the observable that proves the process is
    // gone while the record is still active.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let projected = controller
                .background_status_entries_with_liveness(TaskOwner::Session(session_id.0.as_ref()))
                .await
                .into_iter()
                .find(|observed| observed.entry.id == stale.id)
                .expect("the second background record must be cached");
            if !projected.entry.status.is_active() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the second background task's exec session must end");
    let entry = controller
        .background_status_entries(TaskOwner::Session(session_id.0.as_ref()))
        .await
        .into_iter()
        .find(|entry| entry.id == stale.id)
        .expect("the second background record must stay cached");
    assert!(
        entry.status.is_active(),
        "the controller must still cache the task as active before the read: {entry:?}"
    );
    StaleBackgroundRecord { entry, recorded_output }
}

/// Reads the stale record's output and proves the read neither settles the
/// record nor replaces the launch the controller still caches.
pub(super) async fn read_stale_record_output_is_inert(
    cx: &ConnectionTo<Agent>,
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    workspace_path: &Path,
    notifications: &mut mpsc::UnboundedReceiver<acp::AgentNotification>,
    stale: &StaleBackgroundRecord,
) -> Result<serde_json::Value, acp::Error> {
    // Spawning the task publishes its lifecycle update asynchronously. Settle
    // that traffic before the baseline so the comparison below attributes
    // every difference to the read under test.
    settle_lifecycle_notifications(notifications).await;
    let _ = drain_notifications(notifications);
    let observer = Arc::new(RecordingBackgroundObserver::default());
    let _observer_registration = controller.register_progress_observer(
        session_id.0.to_string(),
        Arc::clone(&observer) as Arc<dyn diapason_core::subagents::SubagentProgressObserver>,
    );
    let _ = observer.take();
    let owner = TaskOwner::Session(session_id.0.as_ref());
    let before = observable_background_snapshot(controller, owner, workspace_path, 0).await;
    let stale_output = lody_output_tail(cx, session_id, &stale.entry.id, 20).await?;
    let published = observer.take();
    let after = observable_background_snapshot(controller, owner, workspace_path, 0).await;
    assert_eq!(
        after, before,
        "an output read of an active record with an exited child must not refresh, settle, respawn, or persist"
    );
    assert!(published.is_empty(), "an output read must not publish a background status update: {published:?}");
    let still_cached = controller
        .background_status_entries(owner)
        .await
        .into_iter()
        .find(|entry| entry.id == stale.entry.id)
        .expect("the second background record must stay cached after the read");
    assert!(
        still_cached.status.is_active(),
        "an output read must not settle a record the controller still caches as active"
    );
    assert_eq!(
        stale_output["output"], stale.recorded_output,
        "an exited task must still serve the output it recorded: {stale_output}"
    );
    match stale.entry.transcript_path.as_ref().or(stale.entry.archive_path.as_ref()) {
        Some(path) => {
            let archived = std::fs::read_to_string(path).expect("read the recorded transcript");
            assert!(
                archived.contains(stale_output["output"].as_str().unwrap_or_default()),
                "a recorded transcript must supply the read output: {stale_output}"
            );
        }
        None => assert!(
            stale_output["output"]
                .as_str()
                .is_some_and(|output| output.contains("lody-output-two")),
            "a task with no recorded transcript must serve its live buffer: {stale_output}"
        ),
    }
    Ok(stale_output)
}
