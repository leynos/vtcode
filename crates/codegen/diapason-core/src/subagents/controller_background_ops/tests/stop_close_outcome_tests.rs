//! Exercises the outcome a stop reports and the ordering of its close attempts.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};

#[cfg(unix)]
use super::super::BackgroundRunRequest;
use super::super::controller_background_cancel::{
    background_stop_close_is_pending, fail_after_gated_background_close, fail_next_background_close,
    gate_next_background_close,
};
#[cfg(unix)]
use super::super::controller_background_run::gate_next_background_commit;
use super::support::{background_record, background_test_controller};
#[cfg(unix)]
use super::support::{session_is_gone, write_background_demo_agent, write_sleeping_demo_script};

/// A failed close must not undo a close another attempt already confirmed.
///
/// Two attempts for one record overlap: the first is stalled inside its close,
/// the second runs and confirms its close, and only then does the first fail.
/// Applying that late failure rewound the record from `Stopped` to `Running`,
/// re-armed an obligation the confirmed close had discharged, and reported a
/// stopped task as live again.
#[tokio::test]
async fn a_late_close_fault_does_not_undo_a_confirmed_close() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-overlapping-close";
    let record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let (first_entered, first_release) =
        fail_after_gated_background_close(&controller.state, exec_session_id, "late close fault");
    let first = controller.attempt_background_stop_close("background-worker", exec_session_id);
    tokio::pin!(first);
    tokio::select! {
        confirmed = &mut first => {
            panic!("the first gated close settled before the overlapping attempt ran: {confirmed}")
        }
        () = first_entered.notified() => {}
    }

    // The second attempt stalls in its own close while the first is still
    // inside the failed one, so both attempts really overlap.
    let (second_entered, second_release) = gate_next_background_close(&controller.state, exec_session_id);
    let second = controller.attempt_background_stop_close("background-worker", exec_session_id);
    tokio::pin!(second);
    tokio::select! {
        confirmed = &mut second => {
            panic!("the second gated close settled before both attempts overlapped: {confirmed}")
        }
        () = second_entered.notified() => {}
    }

    second_release.notify_one();
    assert!(second.await, "the overlapping attempt confirms the session is closed");

    first_release.notify_one();
    assert!(first.await, "a failure the confirmed close overtook still reports the session closed");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Stopped,
        "the late fault must not rewind the confirmed stop"
    );
    assert!(record.error.is_none(), "the confirmed stop carries no close fault: {:?}", record.error);
    drop(state);
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "the late fault must not re-arm the discharged obligation"
    );

    controller.signal_shutdown().await;
}

/// A close that spends the retry allowance must not read as a successful stop.
///
/// Once the allowance is spent the record settles `Error`, but the stop was
/// still returned as an `Ok` entry and the caller discarded it, so the RPC
/// reported success while the process may have been live. The outcome must say
/// the close is unconfirmed and carry the record naming the fault.
#[tokio::test]
async fn a_spent_close_retry_reports_a_pending_stop() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-unconfirmed-close";
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        true,
    );
    record.restart_attempts = 1;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    let outcome = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("the stop is reported as an outcome, not an RPC failure");
    assert!(outcome.is_close_pending(), "an unconfirmed close must not report a completed stop");
    assert_eq!(
        outcome.entry().status,
        BackgroundSubprocessStatus::Error,
        "a spent close retry settles the record as a visible failure"
    );
    assert!(
        outcome
            .entry()
            .error
            .as_deref()
            .is_some_and(|error| error.contains("close transport fault")),
        "the outcome's record names the close fault: {:?}",
        outcome.entry().error
    );
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "a spent allowance discharges the obligation"
    );

    controller.signal_shutdown().await;
}

/// A stop with no session to close is a typed absence, not a pending close.
#[tokio::test]
async fn a_stop_without_a_session_reports_a_confirmed_close() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record("background-worker", None, "worker", "", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let outcome = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("a stop with nothing to close still settles the record");
    assert!(!outcome.is_close_pending(), "a record that names no session owes no close");
    assert_eq!(outcome.entry().status, BackgroundSubprocessStatus::Stopped);
    assert!(!outcome.entry().desired_enabled);

    controller.signal_shutdown().await;
}

/// A declined launch's session is retried until its close succeeds.
///
/// The launch spawned a session and then found its record no longer wanted the
/// commit, so no record names that session. A close fault used to be logged and
/// forgotten. The obligation must be keyed by the launch's own exec session and
/// retried by a reconcile even though the record has moved to another launch.
#[cfg(unix)]
#[tokio::test]
async fn a_declined_launch_close_is_retried_until_it_succeeds() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    let record = background_record(
        "background-worker",
        None,
        "background-demo",
        "exec-previous",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let (entered, release) = gate_next_background_commit(&controller.state);
    let launch = controller.ensure_background_record_running(BackgroundRunRequest {
        agent_name: "background-demo".to_string(),
        stable_id: Some("background-worker".to_string()),
        restart_attempts: 0,
        overrides: None,
        expected_exec_session_id: None,
    });
    tokio::pin!(launch);
    let declined_exec_session_id = tokio::select! {
        result = &mut launch => panic!("the gated commit settled before the record moved on: {result:?}"),
        () = entered.notified() => {
            let mut state = controller.state.write().await;
            let record = state.background_children.get_mut("background-worker").expect("record");
            let spawned_exec_session_id = record.exec_session_id.clone();
            // A replacement launch takes the record while the spawn is in
            // flight, so the commit declines and no record names the session
            // this launch created.
            record.desired_enabled = false;
            record.exec_session_id = "exec-successor".to_string();
            drop(state);
            fail_next_background_close(&controller.state, &spawned_exec_session_id, "declined close transport fault");
            spawned_exec_session_id
        }
    };

    release.notify_one();
    let entry = launch.await.expect("the declined launch reports the record it left");
    assert_eq!(entry.exec_session_id, "exec-successor", "the record keeps the launch that replaced it");
    assert!(
        !session_is_gone(&controller, &declined_exec_session_id).await,
        "the scripted fault leaves the declined launch's session live"
    );
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", &declined_exec_session_id),
        "a declined launch's failed close must stay owed"
    );

    controller
        .retry_pending_background_stop_closes()
        .await
        .expect("the retried close succeeds");
    assert!(
        session_is_gone(&controller, &declined_exec_session_id).await,
        "the retry closes the session no record names"
    );
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", &declined_exec_session_id),
        "a successful retry discharges the detached obligation"
    );

    controller.signal_shutdown().await;
}
