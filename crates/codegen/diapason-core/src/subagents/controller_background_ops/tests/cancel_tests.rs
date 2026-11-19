//! Exercises cancellation settlement and the retry that republishes it.

use std::sync::Arc;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use crate::subagents::background::background_state_path_for_tests;
use tempfile::TempDir;

use super::support::RecordingObserver;
use super::support::background_record;
use super::support::background_test_controller;
#[cfg(unix)]
use super::support::session_is_gone;
#[cfg(unix)]
use super::support::start_sleep_session;

/// A cancellation whose save fails must still publish its stop on the retry.
///
/// The first attempt settles the record in memory and then fails to write the
/// ledger. The retry takes the already-terminal branch, so only publishing the
/// committed snapshot there makes the stop visible; a client would otherwise
/// keep showing the task as running after a stop it was told failed.
#[tokio::test]
async fn a_failed_save_republishes_the_stopped_state_on_retry() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-worker",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    // A directory where the ledger belongs makes the atomic write fail, so the
    // first cancellation settles the record without committing it.
    let ledger = background_state_path_for_tests(temp.path());
    std::fs::create_dir_all(&ledger).expect("the ledger path blocks the write");

    controller
        .force_cancel_background("background-worker")
        .await
        .expect_err("the first cancellation cannot persist its stop");

    std::fs::remove_dir(&ledger).expect("the ledger path is writable again");
    let observer = Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());

    let entry = controller
        .force_cancel_background("background-worker")
        .await
        .expect("the retry persists the stop");

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!entry.desired_enabled);
    assert!(entry.ended_at.is_some(), "the stop keeps the instant it was first recorded");
    let published = observer.background_entries();
    let last = published.last().expect("the retry publishes the settled record");
    assert_eq!(
        last.status,
        BackgroundSubprocessStatus::Stopped,
        "a retried cancellation must publish the state it just committed"
    );
    assert!(!last.desired_enabled, "the published stop must clear the desired flag");

    drop(registration);
    controller.signal_shutdown().await;
}

/// A forced cancellation still closes the session a settled record names.
///
/// Graceful cancellation marks a record `Stopped` before its process leaves,
/// and pruning only removes a session that has already exited, so a settled
/// record can still name a live session. Force must terminate it without
/// rewriting the terminal outcome or its timestamps.
#[cfg(unix)]
#[tokio::test]
async fn force_cancel_closes_the_session_of_an_already_settled_record() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-settled";
    start_sleep_session(&controller, exec_session_id).await;
    let ended_at = "2026-08-28T01:02:03Z"
        .parse::<chrono::DateTime<chrono::Utc>>()
        .expect("ended_at");
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Error,
        true,
    );
    record.error = Some("Background subprocess exited with code 1".to_string());
    record.ended_at = Some(ended_at);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let entry = controller
        .force_cancel_background("background-worker")
        .await
        .expect("force cancel");

    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "a terminal error must not be rewritten as a clean stop"
    );
    assert_eq!(
        entry.error.as_deref(),
        Some("Background subprocess exited with code 1"),
        "the failure text is part of the recorded outcome"
    );
    assert_eq!(entry.ended_at, Some(ended_at), "the recorded end instant survives the cancellation");
    assert!(!entry.desired_enabled, "the stop still has to reach the ledger");
    assert!(
        session_is_gone(&controller, exec_session_id).await,
        "a forced cancellation must close the session a settled record still names"
    );

    controller.signal_shutdown().await;
}

/// A session that already exited is the expected outcome, not a failure.
#[tokio::test]
async fn force_cancel_tolerates_a_session_that_already_exited() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-never-created",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let entry = controller
        .force_cancel_background("background-worker")
        .await
        .expect("a missing session must not fail the cancellation");

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!entry.desired_enabled);

    controller.signal_shutdown().await;
}

/// A stop that was already recorded still terminates its session.
///
/// A graceful stop marks the record `Stopped` before the process leaves, and a
/// stop another controller persisted may never have reached the process, so
/// repeating the cancellation has to close the session this controller can
/// reach. Otherwise a task reported as stopped keeps running.
#[cfg(unix)]
#[tokio::test]
async fn a_recorded_stop_is_terminated_even_on_a_graceful_cancel() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-adopted";
    start_sleep_session(&controller, exec_session_id).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        false,
    );
    record.status = BackgroundSubprocessStatus::Stopped;
    record.ended_at = Some(chrono::Utc::now());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let entry = controller
        .graceful_stop_background("background-worker")
        .await
        .expect("an already-recorded stop is idempotent");

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(
        session_is_gone(&controller, exec_session_id).await,
        "the recorded stop must still terminate the session it names"
    );

    controller.signal_shutdown().await;
}
