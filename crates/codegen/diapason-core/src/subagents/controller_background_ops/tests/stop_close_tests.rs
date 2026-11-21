//! Exercises the close obligation a stop owes its exec session.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_cancel::{
    background_stop_close_is_pending, fail_next_background_close, gate_next_background_close,
};
use super::support::background_record;
use super::support::background_test_controller;
#[cfg(unix)]
use super::support::{session_is_gone, start_sleep_session, write_ledger};

/// A close fault leaves the stop owed, visibly pending, and retryable.
///
/// Reporting `Stopped` before the close succeeds lets the process outlive the
/// stop: refresh skips the record because it is no longer active and restore
/// skips it because it is no longer desired, so nothing retries the close. The
/// record must instead stay in a visible pending state, and a later reconcile
/// with a working close must settle it.
#[cfg(unix)]
#[tokio::test]
async fn a_close_fault_keeps_the_stop_owed_until_a_retry_succeeds() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-close-fault";
    start_sleep_session(&controller, exec_session_id).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("a close fault is reported on the record, not dropped")
        .into_entry();

    assert!(!entry.desired_enabled, "the stop is still intended");
    assert_ne!(
        entry.status,
        BackgroundSubprocessStatus::Stopped,
        "a stop must not be reported complete while its session is unclosed"
    );
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("close transport fault")),
        "the close fault must be visible: {:?}",
        entry.error
    );
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "the failed close must remain owed"
    );
    assert!(
        !session_is_gone(&controller, exec_session_id).await,
        "the fault is injected before the real close, so the process is still live"
    );

    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the retried close succeeds");

    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the retry");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped, "a working close settles the pending stop");
    assert!(entry.error.is_none(), "a settled stop carries no pending close error: {:?}", entry.error);
    assert!(session_is_gone(&controller, exec_session_id).await, "the retry closes the session the stop named");
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "a successful close discharges the obligation"
    );

    controller.signal_shutdown().await;
}

/// An aborted reconcile cannot lose a stop's close obligation.
///
/// The obligation must survive a refresh that is cancelled between starting a
/// close and settling it. Otherwise a process outlives a stop the controller
/// reported, and nothing retries the close. The test gates the close, drops
/// the reconcile future while it waits, then proves the obligation is still
/// owed and that a later reconcile finishes the stop.
#[tokio::test]
async fn an_aborted_reconcile_keeps_the_stop_close_obligation() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-aborted-close";
    let record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    // The first attempt fails, leaving the stop owed.
    fail_next_background_close(&controller.state, exec_session_id, "first close fault");
    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("the failing close leaves a record, not an error")
        .into_entry();
    assert_ne!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id));

    // The retry is gated so the reconcile can be aborted inside the close. The
    // scope is what aborts it: leaving it drops the pinned future rather than
    // merely releasing a reference to it.
    let (entered, _release) = gate_next_background_close(&controller.state, exec_session_id);
    {
        let retry = controller.retry_pending_background_stop_closes();
        tokio::pin!(retry);
        tokio::select! {
            result = &mut retry => panic!("the gated close settled before the reconcile was aborted: {result:?}"),
            () = entered.notified() => {}
        }
    }

    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "an aborted reconcile must not discharge the obligation it never completed"
    );
    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the aborted reconcile");
    assert_ne!(
        entry.status,
        BackgroundSubprocessStatus::Stopped,
        "the aborted close must not leave the stop reported complete"
    );

    // A later reconcile runs the real close, which reports the session gone.
    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the later reconcile finishes the stop");
    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the retry");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id));

    controller.signal_shutdown().await;
}

/// A stop interrupted inside its very first close is still owed.
///
/// The obligation cannot be recorded only after a close fails: a cancellation
/// that is dropped while the close is in flight never reaches that arm, and it
/// has already cleared `desired_enabled`, so neither refresh nor restore looks
/// at the record again. The obligation must exist before the close is awaited.
#[cfg(unix)]
#[tokio::test]
async fn an_interrupted_first_close_is_still_owed() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-interrupted-close";
    start_sleep_session(&controller, exec_session_id).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let (entered, _release) = gate_next_background_close(&controller.state, exec_session_id);
    {
        let cancel = controller.force_cancel_background(TaskOwner::Workspace, "background-worker");
        tokio::pin!(cancel);
        tokio::select! {
            result = &mut cancel => panic!("the gated close settled before the cancellation was interrupted: {result:?}"),
            () = entered.notified() => {}
        }
    }

    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "the stop must be owed before the close completes, not only after it fails"
    );
    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the interruption");
    assert_ne!(
        entry.status,
        BackgroundSubprocessStatus::Stopped,
        "an interrupted close must not leave the stop reported complete"
    );
    assert!(
        !session_is_gone(&controller, exec_session_id).await,
        "the interrupted close never ran, so the process is still live"
    );

    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the next reconcile finishes the interrupted stop");
    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the retry");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(session_is_gone(&controller, exec_session_id).await);
    assert!(!background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id));

    controller.signal_shutdown().await;
}

/// A close fault must not erase the outcome a terminal record already carries.
///
/// A record that settled `Error` keeps its error text and summary: they report
/// the run's failure, and a stop's cleanup fault is additional information, not
/// a replacement. The fault is still visible, and a later reconcile whose close
/// succeeds leaves the original outcome intact.
#[tokio::test]
async fn a_close_fault_preserves_a_terminal_error() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-terminal-fault";
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Error,
        true,
    );
    record.error = Some("Background subprocess exited unexpectedly".to_string());
    record.summary = Some("worker summary".to_string());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "terminal close fault");
    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("the close fault is recorded on the terminal record")
        .into_entry();

    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "the run's terminal outcome must survive its stop's cleanup fault"
    );
    let error = entry.error.as_deref().expect("the terminal record keeps an error");
    assert!(error.contains("Background subprocess exited unexpectedly"), "the original failure is kept: {error}");
    assert!(error.contains("terminal close fault"), "the close fault is added: {error}");
    assert_eq!(entry.summary.as_deref(), Some("worker summary"), "the run's summary is not replaced");
    assert!(background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id));

    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the missing session counts as closed");
    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the retry");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Error);
    assert_eq!(
        entry.error.as_deref(),
        Some(
            "Background subprocess exited unexpectedly; Failed to close the background subprocess's exec session: terminal close fault"
        ),
        "a successful close leaves the terminal outcome, and the recorded fault, intact"
    );
    assert!(!background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id));

    controller.signal_shutdown().await;
}

/// An adopted stop is not terminal until its close has succeeded.
///
/// The adopt path used to write the persisted `Stopped` onto the live record
/// before awaiting the close. A listing then read a stopped task while its
/// process was still running, and the liveness projection skipped the record
/// because it was no longer active. The record must stay active while the
/// close fault stands, keep the obligation owed, and settle `Stopped` only
/// once a retry closes the session.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_stop_is_not_terminal_before_its_close_succeeds() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-adopted-close-fault";
    start_sleep_session(&controller, exec_session_id).await;
    write_ledger(temp.path(), "background-worker", None, "worker", exec_session_id, false, "stopped");
    let record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "adopted close transport fault");

    controller
        .reconcile_persisted_background_cancellations()
        .await
        .expect("reconcile adopts the persisted stop");

    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the adopted record survives the failed close");
    assert!(!entry.desired_enabled, "the adopted stop clears the desired flag");
    assert_ne!(
        entry.status,
        BackgroundSubprocessStatus::Stopped,
        "a live session must not read as stopped before its close succeeds"
    );
    assert!(entry.status.is_active(), "the record stays active while its close is owed: {:?}", entry.status);
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("adopted close transport fault")),
        "the close fault must be visible: {:?}",
        entry.error
    );
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "the failed adopted close must remain owed"
    );
    assert!(
        !session_is_gone(&controller, exec_session_id).await,
        "the fault is injected before the real close, so the process is still live"
    );

    // The scripted fault is one-shot, so the retry runs the real close.
    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the retried close succeeds");

    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("the record survives the retry");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped, "a working close settles the adopted stop");
    assert!(entry.error.is_none(), "a settled stop carries no pending close error: {:?}", entry.error);
    assert!(session_is_gone(&controller, exec_session_id).await, "the retry closes the session the stop named");
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "a successful close discharges the obligation"
    );

    controller.signal_shutdown().await;
}
