//! Exercises restore, refresh, and the authority those paths act under.

use std::sync::Arc;

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;

use super::support::RecordingObserver;
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

/// The claim reconciles the ledger, not a read taken before it.
///
/// The restart's fast path can see no stop and then lose the race: another
/// controller persists one before the replacement is claimed. The claim reads
/// the ledger as part of taking the record, so a stop that lands in that window
/// still prevents the launch instead of being overridden by a newer start.
#[tokio::test]
async fn a_stop_persisted_before_the_claim_prevents_the_replacement() {
    use crate::subagents::controller_background_ops::{BackgroundRunPlan, BackgroundRunReservation};

    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-observed", false, "stopped");
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-observed",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    let plan = BackgroundRunPlan {
        owner_session_id: None,
        spec: diapason_config::builtin_subagents()
            .into_iter()
            .find(|spec| spec.name == "worker")
            .expect("worker spec"),
        record_id: "background-worker".to_string(),
        parent_session_id: "parent-session".to_string(),
        session_id: "child-session".to_string(),
        exec_session_id: "exec-replacement".to_string(),
        created_at: chrono::Utc::now(),
        prompt: "work".to_string(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
        expected_exec_session_id: Some("exec-observed".to_string()),
    };

    let reservation = controller
        .reserve_background_run(&plan)
        .await
        .expect("the claim reconciles the ledger");

    assert!(
        matches!(reservation, BackgroundRunReservation::Declined),
        "a stop persisted before the claim must prevent the replacement"
    );
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert!(!record.desired_enabled, "the stop must be adopted into the record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Stopped);
    assert_eq!(
        record.exec_session_id, "exec-observed",
        "no replacement launch may claim a record another controller stopped"
    );
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

/// Adopting a persisted stop publishes the outcome it adopted.
///
/// The adopted record is already terminal, so refresh will not settle it and
/// restore filters it out of the relaunch set. If the adoption does not
/// publish, an observer is never told the task stopped and keeps reporting it
/// as running.
#[tokio::test]
async fn an_adopted_cancellation_publishes_its_terminal_status() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-adopted",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-adopted", false, "stopped");
    let observer = Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());

    controller
        .restore_background_subagents()
        .await
        .expect("restore adopts the persisted stop");

    let published = observer.background_entries();
    let adopted = published
        .iter()
        .find(|entry| entry.id == "background-worker")
        .expect("the adopted stop is published to observers");
    assert_eq!(
        adopted.status,
        BackgroundSubprocessStatus::Stopped,
        "the published entry carries the status the stop adopted"
    );
    assert!(!adopted.desired_enabled, "the published stop clears the desired flag");

    drop(registration);
    controller.signal_shutdown().await;
}

/// A scheduled restart that cannot prepare settles `Error`.
///
/// The missing-session path leaves the record `Starting` so refresh treats it
/// as an active launch, and `handle_missing_background_snapshot` exempts
/// `Starting` from settlement. If the replacement cannot even be prepared,
/// nothing else drives that state, so the failure must be settled here rather
/// than report a task that is starting forever.
#[tokio::test]
async fn a_restart_that_cannot_prepare_settles_error() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    let record = background_record(
        "background-worker",
        None,
        "no-such-agent",
        "exec-vanished",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-vanished"), None)
        .await
        .expect("the missing session schedules a restart");
    let Some((agent_name, record_id, restart_attempts)) = respawn else {
        panic!("an automatic restore must schedule a replacement");
    };

    let error = controller
        .restart_after_background_observation(&record_id, agent_name, restart_attempts, Some("exec-vanished"))
        .await
        .expect_err("the replacement cannot resolve its agent");

    assert!(format!("{error:#}").contains("no-such-agent"), "{error:#}");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Error,
        "a restart that cannot prepare must not stay Starting"
    );
    assert!(record.error.as_deref().is_some_and(|error| error.contains("no-such-agent")), "{:?}", record.error);
    assert!(record.ended_at.is_some(), "a settled restart failure has ended");
    drop(state);

    controller.signal_shutdown().await;
}

/// A stop persisted after the load is not resurrected by a legacy restore.
///
/// The ACP workspace controller restores ownerless records. Another
/// controller can persist a stop for one of them after this controller loaded
/// its copy, and relaunching that stale copy would undo the stop. The restore
/// must reconcile the persisted ledger before it relaunches anything.
#[tokio::test]
async fn a_stop_persisted_after_load_is_not_resurrected_by_a_legacy_restore() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    // The fixture agent does not exist, so any relaunch attempt would fail:
    // only adopting the persisted stop lets this restore succeed.
    let record = background_record(
        "background-worker",
        None,
        "no-such-agent",
        "exec-stopped",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    // The ledger's agent must canonicalize back to the id the map is keyed by,
    // while the in-memory copy keeps a nonexistent agent so a relaunch attempt
    // fails loudly instead of starting a real process.
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-stopped", false, "stopped");

    controller
        .restore_legacy_background_subagents()
        .await
        .expect("the persisted stop is adopted instead of relaunched");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert!(!record.desired_enabled, "a persisted stop must survive the legacy restore");
    assert_eq!(record.status, BackgroundSubprocessStatus::Stopped);
    drop(state);

    controller.signal_shutdown().await;
}

/// A ledger fault must not skip a close the controller already owes.
///
/// Refresh reconciles persisted cancellations before it retries closes, and the
/// reconciliation read is the step a ledger fault fails. Returning there left a
/// cancelled process running for as long as the ledger stayed unreadable, even
/// though the close retry needs no ledger read of its own. The ledger fault must
/// still reach the caller, so nothing is relaunched without the evidence that
/// read supplies.
#[cfg(unix)]
#[tokio::test]
async fn a_ledger_fault_does_not_skip_a_pending_process_close() {
    use crate::subagents::background::background_state_path_for_tests;

    use super::super::controller_background_cancel::{
        background_stop_close_is_pending, mark_background_stop_close_pending,
    };

    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restoring_config()).await;
    let exec_session_id = "exec-close-despite-ledger";
    start_sleep_session(&controller, exec_session_id).await;
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
    mark_background_stop_close_pending(&controller.state, "background-worker", exec_session_id);

    let ledger_path = background_state_path_for_tests(temp.path());
    std::fs::create_dir_all(ledger_path.parent().expect("ledger parent")).expect("state dir");
    std::fs::write(&ledger_path, "{ this is not a valid background ledger").expect("write corrupt ledger");

    let error = controller
        .refresh_background_processes()
        .await
        .expect_err("the ledger fault still reaches the caller");
    assert!(format!("{error:#}").contains("Failed to parse"), "the caller sees the ledger fault: {error:#}");
    assert!(
        session_is_gone(&controller, exec_session_id).await,
        "a pending close must be retried even when the ledger cannot be read"
    );
    assert!(
        !background_stop_close_is_pending(&controller.state, "background-worker", exec_session_id),
        "the successful close discharges its obligation"
    );

    controller.signal_shutdown().await;
}
