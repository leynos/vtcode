//! Exercises restore, refresh, and the authority those paths act under.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;

use super::support::background_record;
use super::support::background_test_controller;
#[cfg(unix)]
use super::support::session_is_gone;
#[cfg(unix)]
use super::support::start_sleep_session;
use super::support::write_ledger;

/// Enables automatic restore so a skipped record would otherwise be relaunched.
fn restoring_config() -> DiapasonConfig {
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.auto_restore = true;
    vt_cfg
}

/// An adopted cancellation terminates the session the record names.
///
/// The controller that persisted the stop may never have seen the process, so
/// adopting the stop here has to close the session this controller owns.
/// Otherwise restore reports the task stopped while it keeps running.
#[cfg(unix)]
#[tokio::test]
async fn an_adopted_cancellation_terminates_the_recorded_exec_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    let exec_session_id = "exec-adopted";
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

    controller
        .restore_background_subagents()
        .await
        .expect("restore adopts the persisted stop");

    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert!(!record.desired_enabled, "restore must not undo a stop another controller persisted");
        assert_eq!(record.status, BackgroundSubprocessStatus::Stopped);
    }
    assert!(
        session_is_gone(&controller, exec_session_id).await,
        "the controller that adopted the stop must terminate the session it names"
    );

    controller.signal_shutdown().await;
}

/// A persisted stop for the observed launch is adopted instead of relaunched.
#[tokio::test]
async fn a_persisted_stop_for_the_observed_launch_is_carried_into_the_restart() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-observed", false, "stopped");
    // The fixture agent does not exist, so any attempt to relaunch it would
    // fail: only the adopted stop lets this call succeed.
    let record = background_record(
        "background-worker",
        None,
        "no-such-agent",
        "exec-observed",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    controller
        .restart_after_background_observation(
            "background-worker",
            "no-such-agent".to_string(),
            0,
            Some("exec-observed"),
        )
        .await
        .expect("a stopped launch is adopted instead of relaunched");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert!(!record.desired_enabled, "the persisted stop must survive the restart decision");
    assert_eq!(record.status, BackgroundSubprocessStatus::Stopped);
    drop(state);

    controller.signal_shutdown().await;
}

/// A stop naming a replaced session does not cancel this launch.
#[tokio::test]
async fn a_persisted_stop_for_another_launch_does_not_block_the_restart() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-replaced", false, "stopped");
    let record = background_record(
        "background-worker",
        None,
        "no-such-agent",
        "exec-observed",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let error = controller
        .restart_after_background_observation(
            "background-worker",
            "no-such-agent".to_string(),
            0,
            Some("exec-observed"),
        )
        .await
        .expect_err("the restart proceeds for the launch that is still wanted");

    assert!(format!("{error:#}").contains("Failed to resolve background subagent 'no-such-agent'"), "{error:#}");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert!(record.desired_enabled, "a stop naming another launch leaves this one wanted");

    drop(state);

    controller.signal_shutdown().await;
}

/// Restore leaves a record owned by another session alone.
///
/// The unscoped controller loads every record so it can list them, but it
/// cannot see the owning process's exec sessions, so respawning one would race
/// the owner and could start a second process under the same task id.
#[tokio::test]
async fn restore_leaves_a_record_owned_by_another_session_alone() {
    let temp = TempDir::new().expect("tempdir");
    write_ledger(temp.path(), "background-worker", Some("session-owner"), "worker", "exec-foreign", true, "running");
    let controller = background_test_controller(temp.path(), restoring_config()).await;

    let restored = controller
        .restore_background_subagents()
        .await
        .expect("an unscoped restore skips another session's record");

    assert_eq!(restored.len(), 1, "the record is still listed workspace-wide");
    assert_eq!(restored[0].id, "background-worker@session-owner");
    assert_eq!(restored[0].status, BackgroundSubprocessStatus::Running);
    assert!(restored[0].desired_enabled);

    controller.signal_shutdown().await;
}

/// Refresh does not settle another session's running record.
#[tokio::test]
async fn refresh_does_not_settle_another_sessions_record() {
    let temp = TempDir::new().expect("tempdir");
    write_ledger(temp.path(), "background-worker", Some("session-owner"), "worker", "exec-foreign", true, "running");
    let controller = background_test_controller(temp.path(), restoring_config()).await;

    let entries = controller
        .refresh_background_processes()
        .await
        .expect("refresh skips another session's record");

    let entry = entries
        .iter()
        .find(|entry| entry.id == "background-worker@session-owner")
        .expect("the record is still listed");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Running,
        "the owner's record must not be settled from a controller that cannot see its process"
    );
    assert!(entry.desired_enabled);

    controller.signal_shutdown().await;
}
