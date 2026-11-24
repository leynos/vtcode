//! Pins close publication and the exec-session identity a stale stop save
//! carries into the ledger merge.
//!
//! Declared by `controller_spawn_run.rs`; the build fixtures it shares with
//! `tests.rs` live there.

use super::super::background::{load_background_state, persist_background_state};
use super::tests::{child_record, persisted_row, test_config};
use super::*;
use std::sync::Mutex;
use tempfile::TempDir;

/// Records the child snapshots one controller publishes.
#[derive(Default)]
struct ChildStatusObserver {
    entries: Mutex<Vec<SubagentStatusEntry>>,
}

impl SubagentProgressObserver for ChildStatusObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(entry);
    }

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
}

/// Closing a settled child must publish the outcome its abort interrupted.
///
/// The child loop records `Completed` and wakes its waiters before it awaits
/// the publication of that outcome. A close that lands in that window takes
/// the still-live handle and aborts the only publisher, so without publishing
/// here observers keep reporting a running task the caller was already told
/// had finished.
#[tokio::test]
async fn close_publishes_a_terminal_outcome_its_abort_interrupted() {
    let workspace = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf()))
        .await
        .expect("controller");
    let mut record = child_record("agent-worker", SubagentStatus::Running);
    // A pending task gives the close a live handle to abort; it can never
    // finish on its own and race the close.
    record.handle = Some(tokio::spawn(std::future::pending::<()>()));
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    let observer = Arc::new(ChildStatusObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());
    controller.publish_child_status(&id).await;

    // The status is terminal while the handle is still live: the exact window
    // in which the child has recorded its outcome but not published it.
    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut(&id).expect("child record");
        record.status = SubagentStatus::Completed;
        record.completed_at = Some(Utc::now());
    }

    let closed = controller
        .close(TaskOwner::Workspace, &id)
        .await
        .expect("close the settled child");
    assert_eq!(closed.status, SubagentStatus::Completed, "a settled outcome survives the close");

    let published = observer
        .entries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    assert_eq!(
        published.first().map(|entry| entry.status),
        Some(SubagentStatus::Running),
        "the observer first sees the running task"
    );
    assert_eq!(
        published.last().map(|entry| entry.status),
        Some(SubagentStatus::Completed),
        "the close must publish the terminal outcome its abort interrupted: {published:?}"
    );

    drop(registration);
}

/// A stale stop's save must keep a replacement launch another controller
/// wrote.
///
/// The ledger row names `exec-replacement`, while this controller still holds
/// a stopped copy of `exec-stopped` with the newer `updated_at`. The save must
/// carry the stopped launch's exec session into the merge, so stop precedence
/// applies only to the launch it names instead of overwriting the replacement
/// row.
#[tokio::test]
async fn a_stale_stop_save_keeps_a_replacement_launch_in_the_ledger() {
    let workspace = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf()))
        .await
        .expect("controller");
    persist_background_state(
        workspace.path(),
        vec![persisted_row(
            "background-worker",
            None,
            "worker",
            "exec-replacement",
            "running",
            true,
            "2026-08-28T00:00:00Z",
        )],
    )
    .await
    .expect("seed the replacement launch");

    // The stale stop must outrank the persisted row by timestamp, or the merge
    // would keep the replacement for the wrong reason.
    let stale_stop = BackgroundRecord::from_persisted(persisted_row(
        "background-worker",
        None,
        "worker",
        "exec-stopped",
        "stopped",
        false,
        "2030-01-01T00:00:00Z",
    ));
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(stale_stop.id.clone(), stale_stop);
    }

    controller.save_background_state().await.expect("save the stale stop");

    let ledger = load_background_state(workspace.path()).await.expect("read the ledger");
    assert_eq!(ledger.records.len(), 1, "the merge keeps one row per canonical id");
    assert_eq!(
        ledger.records[0].exec_session_id, "exec-replacement",
        "the stale stop must not replace a launch it does not name"
    );
    let persisted = serde_json::to_value(&ledger.records[0]).expect("render the persisted row");
    assert_eq!(persisted["desired_enabled"], serde_json::json!(true), "the replacement launch stays wanted");
}
