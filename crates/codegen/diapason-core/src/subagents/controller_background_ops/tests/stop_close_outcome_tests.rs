//! Exercises the outcome a stop reports and the ordering of its close attempts.

use rstest::rstest;
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
use super::super::controller_background_settlement::pending_background_ledger_write_generation;
use super::support::{background_record, background_test_controller};
#[cfg(unix)]
use super::support::{session_is_gone, write_background_demo_agent, write_sleeping_demo_script};

/// A failed close must not undo a close another attempt already confirmed.
///
/// Two attempts for one record overlap: one is stalled inside its close, the
/// other runs and confirms the close, and only then does the stalled attempt
/// fail. Applying that late failure rewound the record from `Stopped` to
/// `Running`, re-armed an obligation the confirmed close had discharged, and
/// reported a stopped task as live again. The same fixture runs both start
/// orders, so the outcome cannot depend on which attempt took the lower token.
#[rstest]
#[case::faulting_attempt_started_first(false)]
#[case::confirming_attempt_started_first(true)]
#[tokio::test]
async fn a_late_close_fault_does_not_undo_a_confirmed_close(#[case] confirming_attempt_started_first: bool) {
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

    // The first attempt is armed according to the case, started, and held until
    // it is inside its close. The second is armed only then, so the scripted
    // closes are consumed in the order the attempts start.
    let (first_entered, first_release) = if confirming_attempt_started_first {
        gate_next_background_close(&controller.state, exec_session_id)
    } else {
        fail_after_gated_background_close(&controller.state, exec_session_id, "late close fault")
    };
    let first = controller.attempt_background_stop_close("background-worker", exec_session_id);
    tokio::pin!(first);
    tokio::select! {
        confirmed = &mut first => {
            panic!("the first gated close settled before the overlapping attempt ran: {confirmed}")
        }
        () = first_entered.notified() => {}
    }
    let (second_entered, second_release) = if confirming_attempt_started_first {
        fail_after_gated_background_close(&controller.state, exec_session_id, "late close fault")
    } else {
        gate_next_background_close(&controller.state, exec_session_id)
    };
    let second = controller.attempt_background_stop_close("background-worker", exec_session_id);
    tokio::pin!(second);
    tokio::select! {
        confirmed = &mut second => {
            panic!("the second gated close settled before both attempts overlapped: {confirmed}")
        }
        () = second_entered.notified() => {}
    }

    // Release the confirming attempt first, so the fault always lands after the
    // close it must yield to.
    let (confirming, confirming_release, failing, failing_release) = if confirming_attempt_started_first {
        (first, first_release, second, second_release)
    } else {
        (second, second_release, first, first_release)
    };
    confirming_release.notify_one();
    assert!(confirming.await, "the overlapping attempt confirms the session is closed");
    failing_release.notify_one();
    assert!(failing.await, "a failure the confirmed close overtook still reports the session closed");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Stopped,
        "the late fault must not rewind the confirmed stop"
    );
    assert!(record.error.is_none(), "the confirmed stop carries no run error: {:?}", record.error);
    assert!(record.close_fault.is_none(), "the confirmed stop carries no close fault: {:?}", record.close_fault);
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
            .is_none_or(|error| !error.contains("close transport fault")),
        "the close fault is ledger evidence and must not reach a client: {:?}",
        outcome.entry().error
    );
    let durable = {
        let state = controller.state.read().await;
        state
            .background_children
            .get("background-worker")
            .and_then(|record| record.close_fault.clone())
    };
    assert!(
        durable.is_some_and(|fault| fault.contains("close transport fault")),
        "the outcome's record must still carry the fault a spent retry recorded"
    );
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "a spent allowance must not discharge a stop that is still owed"
    );

    controller.signal_shutdown().await;
}

/// A spent close retry owes the ledger the fault it records.
///
/// Once the allowance is spent the record settles `Error` and its close
/// obligation is cleared, so no later pass retries it. The terminal fault then
/// exists only in memory until a save writes it: a caller dropped before that
/// save would leave the ledger describing a completed stop while the process may
/// still be live. The settlement must raise the owed write in the same lock that
/// records the fault.
#[tokio::test]
async fn a_spent_close_retry_owes_the_ledger_the_fault_it_records() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-spent-close";
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Running,
        false,
    );
    record.restart_attempts = 1;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the fixture starts with no pending write, so only the settlement can raise one"
    );

    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");
    assert!(
        !controller
            .attempt_background_stop_close("background-worker", exec_session_id)
            .await,
        "the scripted fault leaves the close unconfirmed"
    );

    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Error,
            "a spent allowance settles the fault as a visible failure"
        );
        assert!(record.ended_at.is_some(), "a settled failure has ended");
    }
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "a spent allowance keeps the close obligation for the next reconcile"
    );
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the settlement must owe the ledger the fault it recorded"
    );

    controller.signal_shutdown().await;
}

/// The default toggle reports a stop whose close it could not confirm.
///
/// The graceful stop answers with the record alone, so the toggle used to return
/// `Ok` while the worker's process could still be live. A user reads the toggle
/// as "the default worker is stopped", so an unconfirmed close must reach the
/// caller as a retryable fault instead of a completed stop.
#[tokio::test]
async fn the_default_toggle_reports_a_stop_it_could_not_close() {
    let temp = TempDir::new().expect("tempdir");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.default_agent = Some("worker".to_string());
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-toggle",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    fail_next_background_close(&controller.state, "exec-toggle", "close transport fault");

    let error = controller
        .toggle_default_background_subagent()
        .await
        .expect_err("an unconfirmed close must not be reported as a completed stop");
    assert!(
        format!("{error:#}").contains("could not be confirmed"),
        "the caller is told the close is still owed: {error:#}"
    );
    assert!(
        background_stop_close_is_pending(&controller.state, "background-worker", "exec-toggle"),
        "the stop keeps its close obligation so a later reconcile retries it"
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
    let error = launch
        .await
        .expect_err("a superseded launch is rejected instead of answering for its replacement");
    assert!(
        format!("{error:#}").contains("was stopped or replaced before its launch could be recorded"),
        "the caller is told its launch never reached a record: {error:#}"
    );
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(record.exec_session_id, "exec-successor", "the record keeps the launch that replaced it");
    }
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
