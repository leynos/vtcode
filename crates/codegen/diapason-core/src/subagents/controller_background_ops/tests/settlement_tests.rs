//! Exercises how a refresh settles a background record.

use chrono::Utc;
use rstest::rstest;
use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundRecord, BackgroundSubprocessStatus, SubagentController};
use crate::tools::types::{DiapasonExecSession, DiapasonSessionLifecycleState};

use super::support::{background_record, background_test_controller};

/// Builds a snapshot for an exited process with the given launch identity.
///
/// The id is a parameter because an observation describes one launch: a test
/// that reports an exit for `exec-old` must hand over a snapshot that also
/// names `exec-old`, or a guard rejecting an inconsistent observation can make
/// the test pass without ever checking what it claims to check.
fn exited_snapshot(exec_session_id: &str) -> DiapasonExecSession {
    DiapasonExecSession {
        id: exec_session_id.to_string().into(),
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
        .update_background_record_state("background-worker", Some("exec-launch"), Some(exited_snapshot("exec-launch")))
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
        .update_background_record_state("background-worker", Some("exec-old"), Some(exited_snapshot("exec-old")))
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

/// The restart allowance is one rule, not two copies of a comparison.
///
/// The read-only projection predicts a replacement from this budget and the
/// settlement path spends it. The table pins both boundaries — automatic
/// restore off, and the single attempt already spent — so a divergence between
/// the two units shows up here rather than as a task that a listing reports as
/// restarting while settlement has already given up (or the reverse).
#[rstest]
#[case::first_attempt_available(true, 0, true)]
#[case::allowance_spent(true, 1, false)]
#[case::several_attempts_spent(true, 2, false)]
#[case::restore_disabled_for_the_first_attempt(false, 0, false)]
#[case::restore_disabled_after_an_attempt(false, 1, false)]
fn the_restart_budget_is_a_single_shared_allowance(
    #[case] auto_restore: bool,
    #[case] restart_attempts: u8,
    #[case] expected: bool,
) {
    assert_eq!(
        super::super::controller_background_settlement::restart_allowed(auto_restore, restart_attempts),
        expected
    );
}

/// A retry clears only the failure it set out to repair.
///
/// The retry saves and then clears, but a second launch can fail its own save
/// while that write is in flight. That newer failure must not be discarded
/// with the one the retry discharged: the next reuse would otherwise report a
/// record the ledger never received as durable. The generation makes the clear
/// a compare-and-clear instead of an unconditional removal.
#[tokio::test]
async fn a_retry_keeps_an_obligation_that_arrived_while_it_was_writing() {
    use super::super::controller_background_settlement::{
        clear_background_ledger_write_pending, mark_background_ledger_write_pending,
        pending_background_ledger_write_generation,
    };

    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    mark_background_ledger_write_pending(&controller.state);
    let observed = pending_background_ledger_write_generation(&controller.state);
    assert_eq!(observed, Some(1), "the first failure creates the obligation");

    // A concurrent launch fails its save while the retry is writing.
    mark_background_ledger_write_pending(&controller.state);
    clear_background_ledger_write_pending(&controller.state, 1);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        Some(2),
        "the retry must not discharge a failure it did not observe"
    );

    clear_background_ledger_write_pending(&controller.state, 2);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        None,
        "the next retry discharges the newer obligation"
    );

    controller.signal_shutdown().await;
}

/// A cleared obligation must not restart the generation it reached.
///
/// The registry used to drop a tree's entry when its last obligation cleared,
/// so the next failure started again at generation 1. A retry that captured
/// that value before the clear could outlive the next mark and acknowledge the
/// new obligation with it; its clear would then remove a write that was still
/// owed, and the next reuse would report an unwritten record as durable. A
/// stale acknowledgement must match nothing once the entry has a newer
/// generation.
#[tokio::test]
async fn a_stale_clear_does_not_discharge_a_later_obligation() {
    use super::super::controller_background_settlement::{
        clear_background_ledger_write_pending, mark_background_ledger_write_pending,
        pending_background_ledger_write_generation,
    };

    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let first = mark_background_ledger_write_pending(&controller.state);
    assert_eq!(first, 1, "the first failure creates the first obligation");
    clear_background_ledger_write_pending(&controller.state, first);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        None,
        "the successful write discharges the obligation it observed"
    );

    let second = mark_background_ledger_write_pending(&controller.state);
    assert_eq!(second, 2, "the generation continues after a cleared obligation");
    // A retry that captured the first generation and clears late must not
    // discharge the obligation raised after it.
    clear_background_ledger_write_pending(&controller.state, first);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        Some(second),
        "a stale acknowledgement must leave the newer obligation in place"
    );

    clear_background_ledger_write_pending(&controller.state, second);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        None,
        "the obligation's own acknowledgement discharges it"
    );

    controller.signal_shutdown().await;
}
