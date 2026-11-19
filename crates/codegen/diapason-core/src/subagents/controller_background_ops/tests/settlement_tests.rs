//! Exercises how a refresh settles a background record.

use chrono::Utc;
use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundRecord, BackgroundSubprocessStatus, SubagentController};
use crate::tools::types::{DiapasonExecSession, DiapasonSessionLifecycleState};

use super::support::{background_record, background_test_controller};

/// Builds a snapshot for an exited process.
fn exited_snapshot() -> DiapasonExecSession {
    DiapasonExecSession {
        id: "exec-launch".to_string().into(),
        backend: "pipe".to_string(),
        command: "sh".to_string(),
        args: Vec::new(),
        working_dir: None,
        rows: None,
        cols: None,
        child_pid: Some(7),
        started_at: None,
        lifecycle_state: Some(DiapasonSessionLifecycleState::Exited),
        exit_code: Some(0),
    }
}

/// Enables the single automatic restart the settlement tests rely on.
fn restarting_config() -> DiapasonConfig {
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.auto_restore = true;
    vt_cfg
}

/// Inserts one record into the controller's state.
async fn insert_record(controller: &SubagentController, record: BackgroundRecord) {
    let mut state = controller.state.write().await;
    state.background_children.insert(record.id.clone(), record);
}

/// A record scheduled for a replacement must not report an end instant.
///
/// The read-only liveness projection passes `ended_at` through unchanged, so a
/// list that sees `Starting` after an exit would otherwise report both a
/// running task and its end time.
#[tokio::test]
async fn a_scheduled_replacement_clears_the_previous_end_instant() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restarting_config()).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    record.ended_at = Some(Utc::now());
    insert_record(&controller, record).await;

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-launch"), Some(exited_snapshot()))
        .await
        .expect("the observed exit settles the record");

    assert!(respawn.is_some(), "the remaining allowance schedules a replacement");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.ended_at, None, "a record reported as running must not carry an end instant");

    drop(state);

    controller.signal_shutdown().await;
}

/// The missing-session restart path applies the same rule.
#[tokio::test]
async fn a_missing_session_scheduled_for_restart_clears_the_previous_end_instant() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restarting_config()).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    record.ended_at = Some(Utc::now());
    insert_record(&controller, record).await;

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-launch"), None)
        .await
        .expect("the missing session settles the record");

    assert!(respawn.is_some(), "the remaining allowance schedules a replacement");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.ended_at, None, "the replacement has not ended either");

    drop(state);

    controller.signal_shutdown().await;
}

/// A snapshot of an older launch must not settle the replacement.
#[tokio::test]
async fn a_snapshot_from_a_replaced_launch_does_not_settle_the_replacement() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restarting_config()).await;
    let record =
        background_record("background-worker", None, "worker", "exec-new", BackgroundSubprocessStatus::Starting, true);
    insert_record(&controller, record).await;

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-old"), Some(exited_snapshot()))
        .await
        .expect("an observation of a replaced launch is not a failure");

    assert!(respawn.is_none(), "the replacement is still starting, not exited");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.ended_at, None);

    drop(state);

    controller.signal_shutdown().await;
}

/// Disables automatic restore, so a vanished process has no replacement.
fn non_restoring_config() -> DiapasonConfig {
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.auto_restore = false;
    vt_cfg
}

/// A wanted record whose process vanished with no retry left settles `Error`.
///
/// `Running` would report a live task that cannot serve output, and `Stopped`
/// would project as a successful completion the user never asked for. The
/// record is still wanted, so the missing process is its failure.
#[tokio::test]
async fn a_missing_process_without_restore_settles_error() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), non_restoring_config()).await;
    insert_record(
        &controller,
        background_record(
            "background-worker",
            None,
            "worker",
            "exec-launch",
            BackgroundSubprocessStatus::Running,
            true,
        ),
    )
    .await;

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-launch"), None)
        .await
        .expect("a missing process settles without faulting the refresh");

    assert!(respawn.is_none(), "no replacement is scheduled when restore is disabled");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Error);
    assert_eq!(record.error.as_deref(), Some("Background subprocess is not running"));
    assert!(record.ended_at.is_some(), "the failed run still records when it ended");
}

/// The single restart allowance is spent, so a second disappearance fails.
#[tokio::test]
async fn a_missing_process_after_the_restart_allowance_settles_error() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), restarting_config()).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    record.restart_attempts = 1;
    insert_record(&controller, record).await;

    let respawn = controller
        .update_background_record_state("background-worker", Some("exec-launch"), None)
        .await
        .expect("a spent allowance settles without faulting the refresh");

    assert!(respawn.is_none(), "the allowance is already spent, so nothing is scheduled");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Error);
    assert_eq!(record.restart_attempts, 1, "a failed settle must not spend another attempt");
}
