//! Pins the record state a retryable close fault leaves behind.
//!
//! Declared by `tests.rs`. The case turns on what a reverted stop must not keep
//! from the terminal state it is leaving.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::background::load_background_state;
use crate::subagents::{BackgroundRecord, BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_cancel::{
    BACKGROUND_CLOSE_FAULT_PREFIX, background_record_close_is_unconfirmed, fail_next_background_close,
};
use super::reserve_support::{plan, worker_spec};
use super::support::{background_record, background_test_controller};

const WORKER: &str = "background-worker";

/// A retryable close fault clears the end instant it reverts.
///
/// A retryable close fault rewinds a `Stopped` record to `Running` so the
/// pending stop stays visible. The record's end instant belongs to the settled
/// state and must go with it: the projection maps `ended_at` without checking
/// status, and the ACP contract reserves an end instant for terminal tasks. A
/// task that is active again must not list as running with an end.
#[tokio::test]
async fn a_retryable_close_fault_clears_the_end_instant_it_reverts() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-reverted-stop";
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    // The adopted stop settled the record before its close was attempted.
    record.ended_at = Some(chrono::Utc::now());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    let confirmed = controller
        .attempt_background_stop_close("background-worker", exec_session_id)
        .await;
    assert!(!confirmed, "a retryable fault leaves the close unconfirmed");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Running,
        "a retryable fault reports the pending stop as still running"
    );
    assert!(
        record.ended_at.is_none(),
        "an active record must not carry the end instant of the stop it reverted: {:?}",
        record.ended_at
    );
    assert_eq!(record.restart_attempts, 1, "the retry allowance stays spent");
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("close transport fault")),
        "the fault stays visible: {:?}",
        record.error
    );
    drop(state);

    controller.signal_shutdown().await;
}

/// A retryable close fault blocks the start another controller requests.
///
/// The fault leaves the saved row active but no longer desired: the stop began,
/// its close did not confirm the process is gone, and the retry allowance is
/// spent on the attempt. The row's status is therefore not `Error`, but the
/// launch is not finished either. A second controller reads only the ledger, so
/// this row is its whole evidence; it must refuse the start rather than spawn a
/// process beside the one that may still be live.
#[tokio::test]
async fn a_retryable_close_fault_blocks_another_controllers_start() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-retryable-fault";
    let record = background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    let outcome = controller
        .force_cancel_background(TaskOwner::Workspace, WORKER)
        .await
        .expect("a retryable close fault is reported, not returned as a stop failure");
    assert!(outcome.is_close_pending(), "the scripted close leaves the stop owing its close");

    let ledger = load_background_state(temp.path()).await.expect("read the saved stop");
    let stored = ledger
        .records
        .iter()
        .find(|row| row.id == WORKER)
        .expect("the stopped row reaches the ledger");
    assert!(!stored.desired_enabled, "the stop clears the durable intent");
    assert!(stored.status.is_active(), "a retryable fault keeps the stop visibly pending: {:?}", stored.status);
    assert!(
        stored
            .error
            .as_deref()
            .is_some_and(|error| error.contains(BACKGROUND_CLOSE_FAULT_PREFIX)),
        "the row carries the close fault: {:?}",
        stored.error
    );

    let other = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let error = other
        .refuse_explicit_start_over_competing_claim(&plan(worker_spec(), "exec-fresh", None))
        .await
        .expect_err("a row whose close is unconfirmed must not be replaced");
    assert!(
        format!("{error:#}").contains("still owes the close"),
        "the second controller is told why it must wait: {error:#}"
    );

    other.signal_shutdown().await;
    controller.signal_shutdown().await;
}

/// A later confirmed close frees a failed row that still carries old fault text.
///
/// A close fault that exhausted its allowance settles `Error` with the fault
/// appended to the run's own failure. A later force cancel can confirm the
/// process is gone; that confirmation, not the retained text, is what a start
/// decision must read, or the row would block a new launch forever.
#[tokio::test]
async fn a_later_confirmed_close_frees_a_failed_row_with_retained_fault_text() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-confirmed-after-fault";
    let run_failure = "Background subprocess exited with code 3";
    let mut record =
        background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Error, false);
    record.error = Some(format!("{run_failure}; {BACKGROUND_CLOSE_FAULT_PREFIX}: close transport fault"));
    record.summary = Some("Background subagent is stopping".to_string());
    record.ended_at = Some(chrono::Utc::now());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    assert!(
        controller.attempt_background_stop_close(WORKER, exec_session_id).await,
        "a session this controller no longer holds is already closed"
    );
    controller.save_background_ledger().await.expect("save the confirmed close");

    let ledger = load_background_state(temp.path()).await.expect("read the settled row");
    let stored = ledger
        .records
        .iter()
        .find(|row| row.id == WORKER)
        .expect("the row reaches the ledger");
    assert_eq!(
        stored.error.as_deref(),
        Some(run_failure),
        "the confirmation removes the close fault and keeps the run's own failure"
    );
    let restored = BackgroundRecord::from_persisted(stored.clone());
    assert!(
        !background_record_close_is_unconfirmed(&restored),
        "a confirmed close frees the row even though the fault text was recorded"
    );

    let other = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    other
        .refuse_explicit_start_over_competing_claim(&plan(worker_spec(), "exec-fresh", None))
        .await
        .expect("a confirmed close no longer blocks a start");

    other.signal_shutdown().await;
    controller.signal_shutdown().await;
}
