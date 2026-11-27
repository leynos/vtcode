//! Pins the record state a retryable close fault leaves behind.
//!
//! Declared by `tests.rs`. The case turns on what a reverted stop must not keep
//! from the terminal state it is leaving.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::background::load_background_state;
use crate::subagents::{BackgroundRecord, BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_cancel::{
    BACKGROUND_CLOSE_FAULT_PREFIX, background_record_close_is_unconfirmed, background_stop_close_is_pending,
    fail_next_background_close,
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
            .close_fault
            .as_deref()
            .is_some_and(|fault| fault.contains("close transport fault")),
        "the fault stays visible: {:?}",
        record.close_fault
    );
    assert!(record.error.is_none(), "the close fault is not a run outcome: {:?}", record.error);
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
            .close_fault
            .as_deref()
            .is_some_and(|fault| fault.contains(BACKGROUND_CLOSE_FAULT_PREFIX)),
        "the row carries the close fault: {:?}",
        stored.close_fault
    );
    assert!(stored.error.is_none(), "the close fault never becomes the run's error: {:?}", stored.error);

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

/// A later confirmed close clears the fault and keeps the run's own failure.
///
/// The fault has its own durable field, so confirming the close removes only
/// that evidence: the `Error` row a spent close retry settled still carries the
/// outcome the run itself reported. A start decision then reads the cleared
/// fault, or the row would block a new launch forever.
#[tokio::test]
async fn a_later_confirmed_close_frees_a_failed_row_and_keeps_its_run_error() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-confirmed-after-fault";
    let run_failure = "Background subprocess exited with code 3";
    let mut record =
        background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Error, false);
    record.error = Some(run_failure.to_string());
    record.close_fault = Some(format!("{BACKGROUND_CLOSE_FAULT_PREFIX}: close transport fault"));
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
        "the confirmation clears the close fault and keeps the run's own failure"
    );
    assert!(stored.close_fault.is_none(), "the confirmed close clears its own evidence");
    let restored = BackgroundRecord::from_persisted(stored.clone());
    assert!(
        !background_record_close_is_unconfirmed(&restored),
        "a confirmed close frees the row even though the fault was recorded"
    );

    let other = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    other
        .refuse_explicit_start_over_competing_claim(&plan(worker_spec(), "exec-fresh", None))
        .await
        .expect("a confirmed close no longer blocks a start");

    other.signal_shutdown().await;
    controller.signal_shutdown().await;
}

/// A genuine run failure that quotes the close-fault wording is not a stop.
///
/// The start predicate used to search the run error for the fault text, so a
/// worker whose own failure mentioned it was treated as a record whose process
/// might still be live. The predicate now reads the separate fault field, and a
/// confirmed close leaves the run's error exactly as it was reported.
#[tokio::test]
async fn a_run_error_quoting_the_close_fault_does_not_block_a_start_or_get_truncated() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-genuine-run-error";
    let run_failure = format!("Worker reported: {BACKGROUND_CLOSE_FAULT_PREFIX} in its own test");
    let mut record =
        background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Error, false);
    record.error = Some(run_failure.clone());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert!(
            !background_record_close_is_unconfirmed(record),
            "a run error is not evidence that this controller still owes a close"
        );
    }

    assert!(
        controller.attempt_background_stop_close(WORKER, exec_session_id).await,
        "a session this controller no longer holds is already closed"
    );

    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.error.as_deref(),
        Some(run_failure.as_str()),
        "a confirmed close must preserve the whole run error"
    );
    assert!(record.close_fault.is_none(), "the genuine error never recorded a close fault");
    drop(state);

    controller.signal_shutdown().await;
}

/// A stop whose close retry allowance is spent is still finished by a later pass.
///
/// The restart allowance governs automatic replacements, not whether a stop the
/// caller requested is ever finished. Clearing the close obligation once the
/// allowance was spent left a possibly live process that no later refresh or
/// restore selected, so the obligation must stay until a close confirms it.
#[tokio::test]
async fn a_spent_close_fault_is_retried_by_the_next_reconcile() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-spent-close";
    let mut record =
        background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Running, false);
    record.restart_attempts = 1;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    assert!(
        !controller.attempt_background_stop_close(WORKER, exec_session_id).await,
        "the scripted fault leaves the close unconfirmed"
    );
    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Error,
            "a spent allowance settles the fault as a visible failure"
        );
        assert!(
            record
                .close_fault
                .as_deref()
                .is_some_and(|fault| fault.contains("close transport fault")),
            "the fault is durable evidence the close is still owed: {:?}",
            record.close_fault
        );
    }
    assert!(
        background_stop_close_is_pending(&controller.state, WORKER, exec_session_id),
        "a spent allowance must not discharge the stop"
    );

    // The next reconcile retries the close with a working session manager and
    // finishes the stop the allowance no longer governs.
    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the retried close succeeds");
    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert!(record.close_fault.is_none(), "the successful retry clears the fault");
    drop(state);
    assert!(
        !background_stop_close_is_pending(&controller.state, WORKER, exec_session_id),
        "only the successful close discharges the obligation"
    );

    controller.signal_shutdown().await;
}
