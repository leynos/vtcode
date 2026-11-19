//! A launch settles only the run it still owns.
//!
//! A launch that never started must settle as a failure rather than stay
//! `Starting` forever, and a launch whose record has since been cancelled or
//! replaced must not overwrite the outcome that record now carries.
use super::background_liveness_support::*;
use super::*;

/// A launch whose exec session never started must settle as an error.
///
/// Refresh treats a `Starting` record as active, and the liveness overlay
/// preserves `Starting` while the launch is desired, so an unsettled failure
/// that no restart will retry would be reported as a running process forever.
/// The default toggle would then route a graceful stop to a process that never
/// started. The spawn failure is deterministic: the demo agent's launch command
/// is a workspace script, and spawning a missing program fails before any
/// process exists.
#[tokio::test]
async fn failed_background_spawn_settles_the_record_as_an_error() {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("remove the launch script so the spawn fails");
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    cfg.subagents.background.auto_restore = false;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let error = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect_err("a missing launch script must fail the spawn");
    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("Failed to spawn background subprocess"),
        "the caller keeps the contextual spawn failure, got: {rendered}"
    );

    let entry = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.agent_name == "background-demo")
        .expect("the failed launch leaves a record");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "a failed launch with no restart left must not stay Starting"
    );
    let message = entry.error.expect("a settled failure must be explained");
    assert!(message.contains("background-demo"), "the failure must name the agent, got: {message}");
    assert!(
        message.contains("failed to spawn pipe session") || message.contains("No such file"),
        "the failure must include the spawn error, got: {message}"
    );
    assert!(entry.ended_at.is_some(), "a settled failure carries a terminal instant");
    assert!(entry.desired_enabled, "settling a failed launch must not clear the desired state");
}

/// An older launch's failure must not settle a record a replacement owns.
///
/// A record id is stable across restarts and shared with the workspace ledger,
/// so the record an older launch wrote at `Starting` can already name the
/// exec session of a replacement launch. Marking the replacement `Error` would
/// also make a later cancellation take the already-terminal branch and skip
/// process termination, so only the launch that still owns the record may
/// settle it.
#[tokio::test]
async fn a_failed_launch_does_not_settle_the_record_a_replacement_owns() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    write_ledger(
        &state_dir.join("background_subagents.json"),
        vec![with_exec_session(
            record_with_restart_state("background-worker", Some("session-a"), "starting", 0),
            "exec-replacement",
        )],
    );
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), false),
        Some("session-a"),
    )
    .await
    .expect("controller");

    controller
        .settle_failed_background_run(&stale_launch_plan("exec-replaced-launch"), &anyhow::anyhow!("spawn failed"))
        .await;

    let entry = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == "background-worker@session-a")
        .expect("owned background record");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Starting,
        "a replaced launch's failure must not settle the replacement as failed"
    );
    assert!(entry.error.is_none(), "the replacement must not inherit another launch's error");
    assert!(entry.desired_enabled, "settling must not clear the replacement's intent");
}

/// A cancelled record is not settled again by the launch that was stopped.
///
/// Cancellation settles the record while its exec session may still be
/// starting. The stopped launch then fails and reaches the same settlement
/// path; rewriting the terminal outcome to `Error` would relabel a deliberate
/// stop as a failure and discard the instant it ended.
#[tokio::test]
async fn a_failed_launch_does_not_settle_a_cancelled_record() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let ended_at = "2026-08-28T00:00:00Z";
    write_ledger(
        &state_dir.join("background_subagents.json"),
        vec![with_exec_session(
            record_with_terminal_state("background-worker", Some("session-a"), "stopped", 0, false, Some(ended_at)),
            "exec-cancelled-launch",
        )],
    );
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), false),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let before = std::fs::read(state_dir.join("background_subagents.json")).expect("read ledger before settlement");

    controller
        .settle_failed_background_run(&stale_launch_plan("exec-cancelled-launch"), &anyhow::anyhow!("spawn failed"))
        .await;

    let entry = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == "background-worker@session-a")
        .expect("owned background record");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped, "a cancelled record keeps its terminal outcome");
    assert!(entry.error.is_none(), "a cancelled record must not acquire a failure message");
    assert_eq!(
        entry.ended_at,
        Some(ended_at.parse().expect("fixture instant")),
        "the stop keeps the instant it ended"
    );
    assert_eq!(
        std::fs::read(state_dir.join("background_subagents.json")).expect("read ledger after settlement"),
        before,
        "a settlement that does not own the record must not write the ledger"
    );
}
