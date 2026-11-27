//! The read-only liveness projection of a background listing.
//!
//! The projection must follow the record's restart budget, report a terminal
//! instant for a settled task, leave an active task's end unset, and never
//! mutate the persisted ledger.
use super::background_liveness_support::*;
use super::*;
use rstest::rstest;

/// The liveness overlay follows the record's restart budget.
///
/// A record that has already spent its one restart, or whose owner disabled
/// automatic restore, is reported as the error a refresh would settle it as,
/// not as a restart that will never happen. A record whose allowance is
/// unspent is about to be restarted, so an exited process is also projected as
/// `Starting`.
///
/// A cached `Starting` status keeps its task active only while this controller
/// still owes that launch. These fixtures restore a `Starting` row from the
/// ledger, so nothing in this process owes it: the claim that would have driven
/// it belonged to a process that is gone, and this read starts nothing. That
/// makes the row an orphaned claim, and the overlay reports what the next
/// refresh will do with it. A spent or disabled budget is the `Error` the
/// settlement will write; an unspent allowance is the `Starting` the refresh
/// will schedule, because reporting an orphan that will be restarted as a
/// terminal failure would show a finished task that then resumes. A `Running`
/// row whose process exited follows the same budget: nothing was ever claimed
/// for it, and an unspent allowance predicts the restart the refresh will
/// schedule. A `Starting` row that is still owed — a claim this controller
/// wrote, or a restart a previous refresh scheduled — stays `Starting` in
/// every case, which `abandoned_start_liveness_tests` pins.
#[rstest]
#[case::exhausted_budget("running", 1, true, BackgroundSubprocessStatus::Error)]
#[case::abandoned_start_with_spent_budget("starting", 1, true, BackgroundSubprocessStatus::Error)]
#[case::available_budget("running", 0, true, BackgroundSubprocessStatus::Starting)]
#[case::orphaned_start_restarts_with_available_budget("starting", 0, true, BackgroundSubprocessStatus::Starting)]
#[case::abandoned_start_without_auto_restore("starting", 0, false, BackgroundSubprocessStatus::Error)]
#[tokio::test]
async fn liveness_projection_follows_the_restart_budget(
    #[case] cached_status: &str,
    #[case] restart_attempts: u8,
    #[case] auto_restore: bool,
    #[case] expected: BackgroundSubprocessStatus,
) {
    let temp = TempDir::new().expect("tempdir");
    let status = projected_liveness_status(
        &temp,
        record_with_restart_state("background-worker", Some("session-a"), cached_status, restart_attempts),
        auto_restore,
    )
    .await;

    assert_eq!(
        status, expected,
        "cached {cached_status} record with {restart_attempts} restart(s) already spent and auto_restore {auto_restore}"
    );
}

/// A projection that settles a terminal status must report a terminal instant.
///
/// The ACP wire contract numbers `endedAtEpochSeconds` for a terminal task and
/// leaves it null only while the task is active. A record whose process exited
/// before any end instant was observed is projected as the error a refresh
/// would settle it as, so the projection supplies the record's own last
/// observed instant. Using that stored instant, rather than the query time,
/// keeps repeated polls of one unchanged record in agreement.
#[tokio::test]
async fn terminal_liveness_projection_reports_the_last_observed_instant() {
    let temp = TempDir::new().expect("tempdir");
    let last_observed: chrono::DateTime<Utc> = "2026-08-28T00:00:00Z".parse().expect("fixture instant");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 1, true, None),
        false,
    )
    .await;

    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "an exited process with a spent restart budget is a failure"
    );
    assert_eq!(entry.updated_at, last_observed);
    assert_eq!(
        entry.ended_at,
        Some(last_observed),
        "a terminal projection must report the record's last observed instant as its end"
    );
}

/// A projection that keeps a task active leaves its end timestamp unset, so a
/// client never reports an active task as already terminal.
#[tokio::test]
async fn active_liveness_projection_leaves_the_end_timestamp_unset() {
    let temp = TempDir::new().expect("tempdir");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 0, true, None),
        true,
    )
    .await;

    assert_eq!(entry.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(entry.ended_at, None, "an active task has no terminal instant");
}

/// A stopped projection also carries the terminal instant: the owner cleared
/// `desired_enabled`, so the exited process is a completed task rather than a
/// failure.
#[tokio::test]
async fn stopped_liveness_projection_carries_the_terminal_instant() {
    let temp = TempDir::new().expect("tempdir");
    let last_observed: chrono::DateTime<Utc> = "2026-08-28T00:00:00Z".parse().expect("fixture instant");
    let entry = projected_liveness_entry(
        &temp,
        record_with_terminal_state("background-worker", Some("session-a"), "running", 0, false, None),
        false,
    )
    .await;

    assert_eq!(entry.status, BackgroundSubprocessStatus::Stopped);
    assert_eq!(
        entry.ended_at,
        Some(last_observed),
        "a stopped projection must report the record's last observed instant as its end"
    );
}

/// The liveness query is a projection: it must not settle the cached record,
/// rewrite the ledger, or otherwise behave like a refresh.
#[tokio::test]
async fn liveness_projection_does_not_mutate_the_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let ledger_path = background_ledger_path(temp.path());
    write_ledger(
        &ledger_path,
        vec![record_with_restart_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
        )],
    );

    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), true),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before query");

    let projected = controller.background_status_entries_with_liveness(TaskOwner::Workspace).await;
    assert_eq!(
        projected.iter().map(|observed| observed.entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Error],
        "the projection reports the settled outcome of an exhausted restart budget"
    );

    let cached = controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(
        cached.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the liveness query must not settle the cached record"
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after query"),
        ledger_before,
        "the liveness query must not rewrite the persisted ledger"
    );
}

/// A snapshot read is a pure query; only the explicitly named refresh settles
/// or persists a record.
///
/// The record is cached as `Running` and its exec session is gone, so a
/// refresh settles it as `Error`. The pure read must report the cached status
/// and leave the ledger untouched, while the refresh form must produce the
/// settled outcome.
#[tokio::test]
async fn background_snapshot_is_pure_and_refresh_is_explicit() {
    let temp = TempDir::new().expect("tempdir");
    let ledger_path = background_ledger_path(temp.path());
    write_ledger(
        &ledger_path,
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
            true,
            None,
        )],
    );
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), true),
        Some("session-a"),
    )
    .await
    .expect("controller");
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before snapshot");

    let cached = controller
        .background_snapshot(TaskOwner::Workspace, "background-worker@session-a")
        .await
        .expect("cached snapshot");
    assert_eq!(
        cached.entry.status,
        BackgroundSubprocessStatus::Running,
        "the pure snapshot must not settle a record"
    );
    assert_eq!(cached.entry.ended_at, None, "a pure read records no terminal instant");
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after snapshot"),
        ledger_before,
        "the pure snapshot must not persist the ledger"
    );

    let refreshed = controller
        .refresh_then_background_snapshot(TaskOwner::Workspace, "background-worker@session-a")
        .await
        .expect("refreshed snapshot");
    assert_eq!(
        refreshed.entry.status,
        BackgroundSubprocessStatus::Error,
        "the refresh form must settle an exited process"
    );
    assert!(refreshed.entry.ended_at.is_some(), "a settled record carries a terminal instant");
}
