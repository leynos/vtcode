//! Pins that the liveness list does not report an abandoned claim as active.
//!
//! Declared by `tests.rs`. `background_status_entries_with_liveness` is a pure
//! read: it never settles a record or starts a launch. A `Starting` record whose
//! launch obligation is gone therefore has nothing that will ever drive it, and
//! projecting it as active reports a task that is not running and never will be.
//! The orphan is left for the refresh path to settle; the list must not claim it
//! is alive in the meantime.

use rstest::rstest;
use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_lifecycle::mark_background_launch_owed;
use super::super::controller_background_settlement::mark_background_ledger_write_pending;
use super::support::{background_record, background_test_controller};

const WORKER: &str = "background-worker";
const WORKER_AGENT: &str = "worker";
const EXEC: &str = "exec-abandoned";

/// A background record's liveness projection follows the launch it is owed.
///
/// One body covers every arm of the projection. `status` and
/// `restart_attempts` are the record as refresh last left it; `auto_restore` is
/// the configuration the projection reads; `obligation` is the launch still
/// owed, as `(restart_attempts, retry_from_refresh)`; and `ledger_write_debt`
/// marks a write the ledger has not received. The debt is deliberately separate
/// from the obligation: one cannot substitute for the other, so an orphan claim
/// with a pending write must still project as settled.
///
/// The cases are the pairs that must stay distinguishable. A claim with its
/// launch in flight is active whichever budget spent it; a claim nothing drives
/// is not, even when automatic restore would permit one, because this read
/// starts nothing; and a missing `Running` record predicts the replacement
/// refresh will schedule.
#[rstest]
#[case::an_abandoned_claim_is_not_active(
    BackgroundSubprocessStatus::Starting,
    0,
    false,
    None,
    true,
    BackgroundSubprocessStatus::Error
)]
#[case::an_in_flight_launch_stays_active(
    BackgroundSubprocessStatus::Starting,
    0,
    false,
    Some((0, false)),
    false,
    BackgroundSubprocessStatus::Starting
)]
#[case::a_restart_with_a_spent_budget_stays_active(
    BackgroundSubprocessStatus::Starting,
    1,
    false,
    Some((1, true)),
    false,
    BackgroundSubprocessStatus::Starting
)]
#[case::a_running_record_follows_the_exit_projection(
    BackgroundSubprocessStatus::Running,
    0,
    false,
    None,
    false,
    BackgroundSubprocessStatus::Error
)]
#[case::an_orphan_is_inactive_while_restore_is_allowed(
    BackgroundSubprocessStatus::Starting,
    0,
    true,
    None,
    false,
    BackgroundSubprocessStatus::Error
)]
#[case::a_missing_running_record_predicts_its_restart(
    BackgroundSubprocessStatus::Running,
    0,
    true,
    None,
    false,
    BackgroundSubprocessStatus::Starting
)]
#[tokio::test]
async fn a_backgrounds_liveness_projection_follows_the_launch_it_owes(
    #[case] status: BackgroundSubprocessStatus,
    #[case] restart_attempts: u8,
    #[case] auto_restore: bool,
    #[case] obligation: Option<(u8, bool)>,
    #[case] ledger_write_debt: bool,
    #[case] expected: BackgroundSubprocessStatus,
) {
    let temp = TempDir::new().expect("tempdir");
    let mut config = DiapasonConfig::default();
    config.subagents.background.auto_restore = auto_restore;
    let controller = background_test_controller(temp.path(), config).await;
    let mut record = background_record(WORKER, None, WORKER_AGENT, EXEC, status, true);
    record.restart_attempts = restart_attempts;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }
    if let Some((attempts, retry_from_refresh)) = obligation {
        mark_background_launch_owed(&controller.state, WORKER, WORKER_AGENT, attempts, EXEC, retry_from_refresh);
    }
    if ledger_write_debt {
        mark_background_ledger_write_pending(&controller.state);
    }

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert_eq!(
        entry.entry.status, expected,
        "the liveness projection must report the phase the record's launch is really in"
    );
    // `BackgroundSubprocessStatus` has `is_active`; a settled projection is the
    // inverse, and only a settled projection carries an end instant.
    if !expected.is_active() {
        assert!(entry.entry.ended_at.is_some(), "a terminal projection carries an end instant");
    }

    controller.signal_shutdown().await;
}
