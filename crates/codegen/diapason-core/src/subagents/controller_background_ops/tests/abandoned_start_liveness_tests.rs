//! Pins how the liveness list projects a background record's launch obligation.
//!
//! Declared by `tests.rs`. `background_status_entries_with_liveness` is a pure
//! read: it never settles a record or starts a launch. A `Starting` record whose
//! launch obligation is gone therefore has nothing in this process driving it,
//! and the orphan is left for the refresh path. The list reports what that
//! refresh would do with it: the failure a spent or disabled budget settles it
//! as, or the `Starting` replacement automatic restore will still schedule.

use rstest::rstest;
use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_lifecycle::mark_background_launch_owed;
use super::super::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
};
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
/// launch in flight is active whichever budget spent it; an abandoned claim with
/// no restart left is the failure the settlement will write; an abandoned claim
/// that automatic restore would still restart is projected as the replacement
/// refresh will schedule, because reporting it as a terminal failure would show
/// a finished task that then resumes; and a missing `Running` record predicts
/// the replacement refresh will schedule.
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
#[case::an_orphan_restart_is_pending_while_restore_is_allowed(
    BackgroundSubprocessStatus::Starting,
    0,
    true,
    None,
    false,
    BackgroundSubprocessStatus::Starting
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
    // The projection is a pure read: whatever it reports, the stored record and
    // its obligation must be exactly as the fixture left them. Without this a
    // projection that settled the record and queued a write would pass on the
    // returned status alone.
    {
        let state = controller.state.read().await;
        let stored = state.background_children.get(WORKER).expect("the record is still stored");
        assert_eq!(stored.status, status, "the read-only projection must not settle the stored record");
        assert_eq!(stored.restart_attempts, restart_attempts, "the projection must not spend the allowance");
        assert!(stored.ended_at.is_none(), "the projection must not write an end instant");
    }
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        ledger_write_debt,
        "the projection must not queue a ledger write of its own"
    );

    controller.signal_shutdown().await;
}
