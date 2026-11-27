//! Pins the stop intents that must survive a dropped or faulted caller.
//!
//! Declared by `tests.rs`. Two transitions are covered here because both are
//! windows in which a stop exists only in memory: clearing the intent of an
//! already-terminal record, and refreshing a settled stop's archive before its
//! save. In each, a fault that skips the save would leave a cancelled task
//! restorable or a confirmed stop unpublished.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::background::{load_background_state, persist_background_state};
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};
use crate::utils::session_archive::override_sessions_dir_for_tests;

use super::super::controller_background_cancel::{
    background_stop_close_is_pending, gate_next_background_cancellation_save, mark_background_stop_close_pending,
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
    // Restore selects on this flag alone, so the durable row must begin
    // enabled; otherwise a later pass that wrote nothing would still leave the
    // assertion below satisfied.
    persist_background_state(temp.path(), vec![record.clone().into_persisted()])
        .await
        .expect("seed the enabled row a resume would restore");
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

    // The obligation is only worth raising if a later pass actually persists
    // the interrupted stop: a marker nothing drives would still pass the
    // assertion above. Refresh is the pass that retries the owed write.
    controller
        .refresh_background_processes()
        .await
        .expect("the next refresh persists the interrupted stop");
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|row| row.id == WORKER)
        .expect("the interrupted stop reaches the ledger");
    assert!(
        !stored.desired_enabled,
        "the reconciliation pass must persist the cleared intent a resume selects on"
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

/// A stop answers with the archive paths the refresh attached.
///
/// The close captures the stop's entry before `finish_background_cancellation`
/// refreshes the archive, so answering with that copy reports no archive while
/// the saved record has one. The refresh's paths belong to the launch the close
/// settled, so the stop re-reads that launch and never a replacement.
#[tokio::test]
async fn a_stop_reports_the_archive_paths_attached_after_its_close() {
    use std::path::PathBuf;

    use super::super::controller_background_archive::attach_background_archive_paths;

    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-archive";
    let record = background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Stopped, false);
    let session_id = record.session_id.clone();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    let closed = controller
        .background_status_for(WORKER)
        .await
        .expect("the launch the close settled");
    assert!(closed.archive_path.is_none(), "the close's copy predates the refresh");

    let archive = PathBuf::from("/archive/session-background-worker.json");
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, WORKER, &session_id, exec_session_id, &archive);
    }
    let reported = controller
        .refreshed_settled_background_stop(WORKER, Some(closed))
        .await
        .expect("the stop reports its own launch");
    assert_eq!(reported.exec_session_id, exec_session_id);
    assert_eq!(reported.archive_path, Some(archive.clone()), "the stop reports the archive the refresh attached");
    assert_eq!(reported.transcript_path, Some(archive), "the transcript path came from the same refresh");

    // A replacement launch is never reported as this stop's outcome.
    let own = controller.background_status_for(WORKER).await.expect("the stop's own entry");
    let replacement = background_record(WORKER, None, "worker", "exec-next", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(replacement.id.clone(), replacement);
    }
    let reported = controller
        .refreshed_settled_background_stop(WORKER, Some(own))
        .await
        .expect("a stop whose launch moved keeps its own entry");
    assert_eq!(reported.exec_session_id, exec_session_id, "a replacement is not the stop's outcome");

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

/// A retry cannot re-arm a close another attempt already confirmed.
///
/// The retry wakes from a snapshot taken without the state lock. A concurrent
/// attempt can confirm the same close in between, and the record still names the
/// same exec session and still does not want it, so the old check passed on the
/// stale snapshot and registered an obligation for a close that had already
/// succeeded. That refused starts until another reconcile happened to clear it.
/// The ownership check now reads the obligation under the same lock the
/// confirmation clears it with.
#[tokio::test]
async fn a_retry_does_not_rearm_a_close_another_attempt_confirmed() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-confirmed-before-retry";
    let record = background_record(WORKER, None, "worker", exec_session_id, BackgroundSubprocessStatus::Running, false);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    mark_background_stop_close_pending(&controller.state, WORKER, exec_session_id);

    // An overlapping attempt confirms the close and discharges the obligation.
    assert!(
        controller.attempt_background_stop_close(WORKER, exec_session_id).await,
        "a session this controller no longer holds is already closed"
    );
    assert!(!background_stop_close_is_pending(&controller.state, WORKER, exec_session_id));

    assert!(
        !controller.retry_background_stop_close(WORKER, exec_session_id).await,
        "a retry from a stale snapshot must not attempt a confirmed close again"
    );
    assert!(
        !background_stop_close_is_pending(&controller.state, WORKER, exec_session_id),
        "the retry must not re-register an obligation the confirmation discharged"
    );

    controller.signal_shutdown().await;
}
