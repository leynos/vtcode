//! Pins that the liveness list does not report an abandoned claim as active.
//!
//! Declared by `tests.rs`. `background_status_entries_with_liveness` is a pure
//! read: it never settles a record or starts a launch. A `Starting` record whose
//! launch obligation is gone therefore has nothing that will ever drive it, and
//! projecting it as active reports a task that is not running and never will be.
//! The orphan is left for the refresh path to settle; the list must not claim it
//! is alive in the meantime.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, TaskOwner};

use super::super::controller_background_lifecycle::mark_background_launch_owed;
use super::super::controller_background_settlement::mark_background_ledger_write_pending;
use super::support::{background_record, background_test_controller};

const WORKER: &str = "background-worker";
const WORKER_AGENT: &str = "worker";
const EXEC: &str = "exec-abandoned";

/// A locally driven `Starting` record with no obligation is not active.
///
/// The claim a launch writes is `Starting` with its obligation recorded. When
/// that driver disappears the obligation is cleared but the record keeps the
/// status until a refresh settles it. The list must not read that window as a
/// live task: the exec session never registered, and nothing will start one.
#[tokio::test]
async fn a_starting_record_without_a_launch_obligation_is_not_active() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(WORKER, None, WORKER_AGENT, EXEC, BackgroundSubprocessStatus::Starting, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }
    // The claim still owes its write, so the projection is not answering from a
    // record the ledger has received either.
    mark_background_ledger_write_pending(&controller.state);

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert!(
        !entry.entry.status.is_active(),
        "an abandoned Starting claim must not be projected as an active launch: {:?}",
        entry.entry.status
    );

    controller.signal_shutdown().await;
}

/// A `Starting` record with an in-flight launch is still active.
///
/// The positive control: the same fixture with its obligation recorded is the
/// launch the caller is waiting for, and it must stay `Starting` rather than be
/// reported as a failure while the process is still coming up.
#[tokio::test]
async fn a_starting_record_with_an_in_flight_launch_stays_active() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(WORKER, None, WORKER_AGENT, EXEC, BackgroundSubprocessStatus::Starting, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }
    mark_background_launch_owed(&controller.state, WORKER, WORKER_AGENT, 0, EXEC, false);

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Starting,
        "a claim with its launch still in flight must stay Starting"
    );
    assert!(entry.entry.status.is_active(), "the launch is active while it is still owed");

    controller.signal_shutdown().await;
}

/// A restart in flight stays active after the budget is spent.
///
/// Refresh increments the restart counter before the replacement exec session is
/// registered, so a record can be `Starting` with its one allowance already
/// spent and a replacement on its way. The obligation is what distinguishes
/// that from an abandoned claim with the same counters: the positive control
/// above covers a fresh start, and this covers the restart, so the projection
/// cannot be narrowed to "an unspent budget" without failing one of them.
#[tokio::test]
async fn a_starting_restart_with_a_spent_budget_stays_active() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let mut record = background_record(WORKER, None, WORKER_AGENT, EXEC, BackgroundSubprocessStatus::Starting, true);
    record.restart_attempts = 1;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }
    mark_background_launch_owed(&controller.state, WORKER, WORKER_AGENT, 1, EXEC, true);

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Starting,
        "a restart still in flight must stay Starting however its budget was spent"
    );

    controller.signal_shutdown().await;
}

/// A running record is unaffected by the obligation rule.
///
/// The rule narrows one projection arm only. A `Running` record with no
/// obligation, whose session read fails as missing, still follows the existing
/// path rather than being confused with an abandoned claim.
#[tokio::test]
async fn a_running_record_is_untouched_by_the_launch_obligation_rule() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record =
        background_record(WORKER, None, WORKER_AGENT, "exec-running", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");
    // Auto-restore is off by default, so the missing session settles it.
    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Error,
        "a Running record with no obligation follows the ordinary exit projection"
    );

    controller.signal_shutdown().await;
}

/// An orphaned claim stays inactive even while automatic restore is enabled.
///
/// The restart budget is not a driver. With `auto_restore` enabled and an
/// unspent attempt, the projection used to report an obligation-free
/// `Starting` claim as active because `restart_allowed` was consulted for every
/// status. This read starts nothing, so it must leave the orphan to the refresh
/// path and not claim it is alive in the meantime.
#[tokio::test]
async fn an_orphaned_claim_is_not_active_when_auto_restore_is_enabled() {
    let temp = TempDir::new().expect("tempdir");
    let mut config = DiapasonConfig::default();
    config.subagents.background.auto_restore = true;
    let controller = background_test_controller(temp.path(), config).await;
    let record = background_record(WORKER, None, WORKER_AGENT, EXEC, BackgroundSubprocessStatus::Starting, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Error,
        "an orphaned claim must not be projected as active merely because a restart is allowed"
    );

    controller.signal_shutdown().await;
}

/// A missing `Running` record still predicts the restart its budget allows.
///
/// The separation the orphan rule needs: a process observed gone with an
/// unspent allowance is a launch refresh will replace, so it keeps projecting
/// `Starting`. Only an obligation-free `Starting` claim is left to settlement.
#[tokio::test]
async fn a_missing_running_record_predicts_its_permitted_restart() {
    let temp = TempDir::new().expect("tempdir");
    let mut config = DiapasonConfig::default();
    config.subagents.background.auto_restore = true;
    let controller = background_test_controller(temp.path(), config).await;
    let record =
        background_record(WORKER, None, WORKER_AGENT, "exec-vanished", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }

    let entries = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    let entry = entries
        .iter()
        .find(|entry| entry.entry.id == WORKER)
        .expect("the record is listed");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Starting,
        "a permitted restart is still predicted for an exited launch"
    );

    controller.signal_shutdown().await;
}
