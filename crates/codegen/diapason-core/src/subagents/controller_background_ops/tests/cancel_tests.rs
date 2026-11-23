//! Exercises cancellation settlement and the retry that republishes it.

use std::sync::Arc;

use crate::config::DiapasonConfig;
use crate::subagents::background::{background_state_path_for_tests, load_background_state};
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};
use tempfile::TempDir;

use super::super::controller_background_cancel::gate_next_background_cancellation_save;
use super::super::controller_background_settlement::pending_background_ledger_write_generation;
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
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect_err("the first cancellation cannot persist its stop");

    // The close itself succeeded, so the in-memory record is already the
    // settled stop. Capture the instant it was first recorded: the retry must
    // republish that same stop, not settle a second time.
    let first_ended_at = {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Stopped,
            "the first cancellation settles the record before its save fails"
        );
        assert!(!record.desired_enabled, "the first cancellation clears the desired flag");
        record.ended_at.expect("a settled stop records when it ended")
    };

    std::fs::remove_dir(&ledger).expect("the ledger path is writable again");
    let observer = Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());

    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("the retry persists the stop")
        .into_entry();

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!entry.desired_enabled);
    assert_eq!(entry.ended_at, Some(first_ended_at), "the retry preserves the instant the stop was first recorded");
    let published = observer.background_entries();
    let last = published.last().expect("the retry publishes the settled record");
    assert_eq!(
        last.status,
        BackgroundSubprocessStatus::Stopped,
        "a retried cancellation must publish the state it just committed"
    );
    assert!(!last.desired_enabled, "the published stop must clear the desired flag");

    // The snapshots above describe in-memory state the first attempt already
    // settled, so the retry's save could be deleted with them still green. The
    // ledger is the durable contract: the retry is the only publication of the
    // stop, so a resumed session must find it committed.
    let persisted = load_background_state(temp.path())
        .await
        .expect("reload the ledger after the retry");
    let persisted_entry = persisted
        .records
        .into_iter()
        .find(|record| record.id == "background-worker")
        .map(crate::subagents::types::BackgroundRecord::from_persisted)
        .expect("the retry commits the record its first attempt settled");
    assert_eq!(persisted_entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!persisted_entry.desired_enabled, "the committed stop must clear the desired flag");

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
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("force cancel")
        .into_entry();

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
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("a missing session must not fail the cancellation")
        .into_entry();

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
        .graceful_stop_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("an already-recorded stop is idempotent");

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(
        session_is_gone(&controller, exec_session_id).await,
        "the recorded stop must still terminate the session it names"
    );

    controller.signal_shutdown().await;
}

/// A stop interrupted during its archive refresh still reaches the ledger.
///
/// The close has already discharged the stop's obligation by the time the
/// archive metadata refresh is awaited, so the settled record exists only in
/// memory. A caller dropped in that window would otherwise leave the stop
/// unwritten, and nothing else would publish it. The obligation is raised
/// before the await, so the next refresh retries the write.
#[tokio::test]
async fn a_stop_interrupted_before_its_save_is_written_by_the_next_refresh() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-never-created",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let (entered, _release) = gate_next_background_cancellation_save(&controller.state);
    {
        let cancel = controller.force_cancel_background(TaskOwner::Workspace, "background-worker");
        tokio::pin!(cancel);
        tokio::select! {
            result = &mut cancel => panic!("the gated stop saved before it was dropped: {result:?}"),
            () = entered.notified() => {}
        }
    }

    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Stopped,
            "the close settled the stop before the refresh that was interrupted"
        );
        assert!(!record.desired_enabled);
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "an interrupted stop must owe the ledger the write its close settled"
    );

    controller
        .refresh_background_processes()
        .await
        .expect("the next refresh writes the owed stop");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the retried write discharges the obligation"
    );
    let persisted = load_background_state(temp.path())
        .await
        .expect("reload the ledger after the retry");
    let stored = persisted
        .records
        .into_iter()
        .find(|record| record.id == "background-worker")
        .map(crate::subagents::types::BackgroundRecord::from_persisted)
        .expect("the interrupted stop reaches the ledger");
    assert_eq!(stored.status, BackgroundSubprocessStatus::Stopped);
    assert!(!stored.desired_enabled, "the written stop clears the desired flag");

    controller.signal_shutdown().await;
}

/// A stop answers for the launch it closed, not one reserved after it.
///
/// A record id outlives one launch. Re-resolving the id after the close can
/// find a replacement another caller has reserved in that window and report it
/// as the stopped launch, so an adapter would announce a finished task whose
/// replacement is still starting.
#[tokio::test]
async fn a_stop_reports_the_launch_it_closed_after_a_replacement_reserves() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-stopped",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    // The replacement reserves the record after the close settled the stop but
    // before the stop answers its caller.
    let (entered, release) = gate_next_background_cancellation_save(&controller.state);
    let stop = controller.force_cancel_background(TaskOwner::Workspace, "background-worker");
    tokio::pin!(stop);
    tokio::select! {
        result = &mut stop => panic!("the gated stop answered before the replacement landed: {result:?}"),
        () = entered.notified() => {}
    }

    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut("background-worker").expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Stopped,
            "the close settled the stop before the replacement arrived"
        );
        let replacement = background_record(
            "background-worker",
            None,
            "worker",
            "exec-replacement",
            BackgroundSubprocessStatus::Starting,
            true,
        );
        state.background_children.insert(replacement.id.clone(), replacement);
    }
    release.notify_one();

    let entry = stop.await.expect("the stop answers after its save").into_entry();
    assert_eq!(
        entry.exec_session_id, "exec-stopped",
        "the stop must report the launch it closed, not the replacement now holding the id"
    );
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert!(!entry.desired_enabled);

    controller.signal_shutdown().await;
}
