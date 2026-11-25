//! Pins the stop intents that must survive a dropped or faulted caller.
//!
//! Declared by `tests.rs`. Two transitions are covered here because both are
//! windows in which a stop exists only in memory: clearing the intent of an
//! already-terminal record, and refreshing a settled stop's archive before its
//! save. In each, a fault that skips the save would leave a cancelled task
//! restorable or a confirmed stop unpublished.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::background::load_background_state;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};
use crate::utils::session_archive::override_sessions_dir_for_tests;

use super::super::controller_background_cancel::{
    background_stop_close_is_pending, fail_after_gated_background_close, gate_next_background_cancellation_save,
    gate_next_background_close,
};
use super::super::controller_background_settlement::{
    pending_background_ledger_write_generation, retry_attempt_available,
};
use super::support::{RecordingObserver, background_record, background_test_controller};

const WORKER: &str = "background-worker";

/// A session-directory override that is cleared when the test ends, even on panic.
struct SessionsDirOverride;

impl Drop for SessionsDirOverride {
    fn drop(&mut self) {
        crate::utils::session_archive::clear_sessions_dir_override_for_tests();
    }
}

/// Clearing a terminal record's intent is owed to the ledger before the close.
///
/// Restore selection reads only `desired_enabled`, so a settled record whose
/// flag is cleared in memory and then never saved is relaunched on the next
/// resume — the exact task the owner just stopped. The obligation must exist in
/// the same lock that clears the flag, so an interrupted caller leaves a write
/// the next refresh retries.
#[tokio::test]
async fn stopping_an_already_terminal_record_owes_its_cleared_intent() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let mut record =
        background_record(WORKER, None, "worker", "exec-terminal", BackgroundSubprocessStatus::Stopped, true);
    record.error = Some("Background subprocess exited unexpectedly".to_string());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the fixture begins with nothing owed"
    );

    controller
        .force_cancel_background(TaskOwner::Workspace, WORKER)
        .await
        .expect("stopping a settled record succeeds");

    // The stop's own save ran, so the retried flag is only observable when a
    // caller is dropped first. Assert the flag itself reached the ledger, which
    // is what the obligation exists to protect.
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|stored| stored.id == WORKER)
        .expect("the stopped record reaches the ledger");
    assert!(!stored.desired_enabled, "a settled stop must clear the durable intent");
    assert_eq!(
        stored.status,
        BackgroundSubprocessStatus::Stopped,
        "the stop must preserve the settled outcome rather than rewriting it"
    );
    assert_eq!(
        stored.error.as_deref(),
        Some("Background subprocess exited unexpectedly"),
        "the stop must not overwrite the run's own failure"
    );

    controller.signal_shutdown().await;
}

/// The cleared intent alone is enough to owe a write.
///
/// The obligation is raised by the cancellation step itself, not by the close.
/// This pins that the marker is created even when the launch names a session
/// that already counts as closed, which is the branch that previously owed
/// nothing at all.
#[tokio::test]
async fn clearing_a_terminal_intent_raises_the_write_obligation() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(WORKER, None, "worker", "", BackgroundSubprocessStatus::Error, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    // No session to close, so only the intent transition can owe a write. The
    // gate parks the caller between the settlement and its save, which is the
    // window the obligation has to cover.
    let (entered, _release) = gate_next_background_cancellation_save(&controller.state);
    {
        let stop = controller.force_cancel_background(TaskOwner::Workspace, WORKER);
        tokio::pin!(stop);
        tokio::select! {
            settled = &mut stop => panic!("the gated stop saved before it was dropped: {settled:?}"),
            () = entered.notified() => {}
        }
    }

    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the cleared intent must owe the ledger the write that reaches restore selection"
    );

    controller.signal_shutdown().await;
}

/// An archive fault must not skip the save and publication of a stop.
///
/// `finish_background_cancellation` refreshes the archive between the close and
/// the save. The archive only supplies projection paths, so its fault must be
/// reported and the pass must carry on: returning it left a confirmed stop
/// unsaved and unpublished for as long as the archive stayed unreadable, which
/// is forever for a malformed file.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn an_archive_fault_does_not_lose_a_confirmed_stop() {
    let temp = TempDir::new().expect("tempdir");
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    // The record's session id resolves to this malformed archive, so the
    // refresh fails every time it runs.
    std::fs::write(sessions_dir.join(format!("session-{WORKER}.json")), "not json")
        .expect("write the unreadable archive");
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionsDirOverride;

    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let observer = std::sync::Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());
    let record =
        background_record(WORKER, None, "worker", "exec-unreadable-archive", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, WORKER)
        .await
        .expect("an archive fault must not fail the stop itself")
        .into_entry();
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped, "the confirmed close still settles the stop");
    assert!(!entry.desired_enabled);

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|stored| stored.id == WORKER)
        .expect("the confirmed stop reaches the ledger despite the archive fault");
    assert_eq!(stored.status, BackgroundSubprocessStatus::Stopped);
    assert!(!stored.desired_enabled, "the durable stop clears the intent restore reads");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the successful save discharges the obligation it covered"
    );
    let published = observer.background_entries();
    assert!(
        published
            .iter()
            .any(|entry| entry.id == WORKER && entry.status == BackgroundSubprocessStatus::Stopped),
        "the confirmed stop must reach the record's observers: {published:?}"
    );

    drop(registration);
    controller.signal_shutdown().await;
}

/// A failure that began before a confirmation still yields to it.
///
/// This is the ordering the token comparison missed: the attempt that will fail
/// starts first and holds the lower token, the confirming attempt runs while it
/// is stalled, and only then does the first fail. Any confirmed close must make
/// a later failure stale whatever tokens the attempts took, or the late fault
/// rewinds `Stopped` to `Running` and re-arms a discharged obligation.
#[tokio::test]
async fn a_failure_started_before_a_confirmation_still_yields_to_it() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-late-failure";
    let record = background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Running, false);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    // The attempt that will fail starts first and stalls inside its close.
    let (failing_entered, failing_release) =
        fail_after_gated_background_close(&controller.state, exec_session_id, "late failure fault");
    let failing = controller.attempt_background_stop_close(WORKER, exec_session_id);
    tokio::pin!(failing);
    tokio::select! {
        settled = &mut failing => panic!("the gated failure settled before the confirming attempt ran: {settled}"),
        () = failing_entered.notified() => {}
    }

    // The confirming attempt completes while the first is stalled. Its session
    // is unknown to this controller, which counts as closed.
    assert!(
        controller.attempt_background_stop_close(WORKER, exec_session_id).await,
        "the overlapping attempt confirms the session is closed"
    );
    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(record.status, BackgroundSubprocessStatus::Stopped, "the confirmed close settles the stop");
    }

    failing_release.notify_one();
    assert!(failing.await, "a failure that began before the confirmation still reports the session closed");

    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Stopped,
        "a failure started before the confirmation must not rewind the stop"
    );
    assert!(record.error.is_none(), "the confirmed stop carries no close fault: {:?}", record.error);
    drop(state);
    assert!(
        !background_stop_close_is_pending(&controller.state, WORKER, exec_session_id),
        "the stale failure must not re-arm the discharged obligation"
    );

    controller.signal_shutdown().await;
}

/// The retry allowance is what decides a stop's ordinary failure path.
///
/// The archive case above must not weaken the rule it sits beside: a close
/// fault is still recorded and retried while the allowance remains, so this
/// pins the shared budget both callers read.
#[test]
fn the_stop_retry_allowance_is_shared_with_restart_settlement() {
    assert!(retry_attempt_available(0), "a first close fault is retryable");
    assert!(!retry_attempt_available(1), "a spent allowance settles instead");
}

/// A failure that starts after a confirmation still yields to it.
///
/// Both attempts are started before either close completes; the attempt that
/// started first takes the lower token and confirms the close, and the attempt
/// that started second takes the higher token and fails only afterwards. A
/// check that compared the failure's token with the confirmed one would treat
/// that failure as newer than the confirmation and rewind the record. The
/// confirmation alone decides, so the record stays stopped and the discharged
/// close obligation is not re-armed.
#[tokio::test]
async fn a_failure_that_started_after_a_confirmation_still_yields_to_it() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-confirmed-first";
    let record = background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Running, false);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    // The confirming attempt starts first and is parked inside its close. The
    // failing attempt starts while it is parked, so it takes the higher token,
    // and is released only after the confirmation.
    let (first_entered, first_release) = gate_next_background_close(&controller.state, exec_session_id);
    let (second_entered, second_release) =
        fail_after_gated_background_close(&controller.state, exec_session_id, "late failure fault");
    let first = controller.attempt_background_stop_close(WORKER, exec_session_id);
    tokio::pin!(first);
    tokio::select! {
        settled = &mut first => panic!("the gated confirmation settled before the second attempt started: {settled}"),
        () = first_entered.notified() => {}
    }
    let second = controller.attempt_background_stop_close(WORKER, exec_session_id);
    tokio::pin!(second);
    tokio::select! {
        settled = &mut second => panic!("the gated failure settled before the confirmation: {settled}"),
        () = second_entered.notified() => {}
    }

    first_release.notify_one();
    assert!(first.await, "the first attempt confirms the close");
    second_release.notify_one();
    assert!(second.await, "a failure that started after the confirmation still reports the session closed");

    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Stopped,
        "a failure started after the confirmation must not rewind the stop"
    );
    assert!(record.error.is_none(), "the confirmed stop carries no close fault: {:?}", record.error);
    drop(state);
    assert!(
        !background_stop_close_is_pending(&controller.state, WORKER, exec_session_id),
        "the late failure must not re-arm the discharged obligation"
    );

    controller.signal_shutdown().await;
}
