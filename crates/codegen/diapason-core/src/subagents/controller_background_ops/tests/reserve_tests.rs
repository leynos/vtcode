//! Pins the reservation step that makes one launch the record's owner.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::SubagentInvocationContext;
use crate::subagents::background::{load_background_state, persist_background_state};
use crate::subagents::background_ledger::{MigratedBackgroundLaunch, persist_background_ledger};

use super::super::BackgroundRunReservation;
use super::reserve_support::{
    OBSERVED_EXEC, WORKER, WORKER_AGENT, insert_observed_record, owned_row_id, persisted_row, plan, worker_spec,
};
use super::support::background_test_controller;

/// A migrated launch is declined under the id its caller still holds.
///
/// The observed session identifies the launch, not the newly generated one.
/// Reading the ledger for the plan's own session made the migration scan
/// unable to match anything, so the reservation replaced a launch that had
/// merely moved to its owner's canonical id.
#[tokio::test]
async fn a_migrated_launch_is_declined_under_the_ownerless_id() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    persist_background_state(
        temp.path(),
        vec![persisted_row(
            &owned_row_id("session-a"),
            Some("session-a"),
            WORKER_AGENT,
            OBSERVED_EXEC,
            true,
        )],
    )
    .await
    .expect("seed the migrated ledger row");

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", Some(OBSERVED_EXEC)))
        .await
        .expect("the migrated launch is declined, not an error");

    assert!(
        matches!(reservation, BackgroundRunReservation::Declined),
        "a launch persisted under its owner's id must not be replaced under the ownerless id"
    );
    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert!(
        ledger.records.iter().all(|record| record.exec_session_id != "exec-replacement"),
        "a declined reservation must not write a replacement claim"
    );

    controller.signal_shutdown().await;
}

/// A claim another controller persisted prevents a second reservation.
///
/// Two controllers loaded the same ledger before either reserved. The first
/// claims the record and writes the `Starting` claim while the ledger lock is
/// held; the second still holds a stale in-memory copy naming the observed
/// session. Without either the durable claim or the ledger check, both would
/// spawn a replacement for the same record.
#[tokio::test]
async fn a_claim_another_controller_persisted_prevents_a_second_reservation() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![persisted_row(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, true)])
        .await
        .expect("seed the observed launch");
    let first = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let second = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    // Both controllers loaded the ledger before either reserved, so each holds
    // the same observed launch while only one of them may replace it.
    insert_observed_record(&first).await;
    insert_observed_record(&second).await;

    let claimed = first
        .reserve_background_run(&plan(worker_spec(), "exec-first", Some(OBSERVED_EXEC)))
        .await
        .expect("the first controller claims the record");
    assert!(matches!(claimed, BackgroundRunReservation::Reserved), "the first reservation owns the record");

    let ledger = load_background_state(temp.path()).await.expect("read the persistent claim");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some("exec-first"),
        "the claim must reach the ledger before the ledger lock is released"
    );

    let contested = second
        .reserve_background_run(&plan(worker_spec(), "exec-second", Some(OBSERVED_EXEC)))
        .await
        .expect("a claim another controller persisted is declined, not an error");
    assert!(
        matches!(contested, BackgroundRunReservation::Declined),
        "a record another controller has claimed must not be claimed again"
    );
    let ledger = load_background_state(temp.path())
        .await
        .expect("read the ledger after the second attempt");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some("exec-first"),
        "the second reservation must not overwrite the persisted claim"
    );

    first.signal_shutdown().await;
    second.signal_shutdown().await;
}

/// A launch started by another invocation's parent is refused for reuse.
///
/// The pre-lock reuse check refuses a record another invocation's parent owns,
/// because reusing it would report another session's task as this one's. The
/// atomic reservation must apply the same rule; otherwise a launch that passed
/// the pre-lock check could still answer for the other parent's record.
#[tokio::test]
async fn a_reuse_from_another_invocation_parent_is_refused() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    let context = SubagentInvocationContext::new("other-parent".to_string(), Vec::new(), "spawn".to_string());

    let error = crate::subagents::with_subagent_invocation_context(
        context,
        controller.reserve_background_run(&plan(worker_spec(), "exec-replacement", None)),
    )
    .await
    .expect_err("another invocation parent must not reuse the record");

    assert!(
        format!("{error:#}").contains("already active"),
        "the reservation must refuse it with the same message the reuse check uses: {error:#}"
    );

    controller.signal_shutdown().await;
}

/// A restart of a launch that moved to an owner is declined after its row was replaced.
///
/// A migration persists the launch under the owner's canonical id and records
/// that it did so. Once the owner replaces the launch, no row names the session
/// this controller observed, but the retained migration evidence still does.
/// Without it the restart would read the ledger, find nothing under the
/// ownerless id and nothing naming its observation, and claim a new launch
/// beside the replacement the owner now runs.
#[tokio::test]
async fn a_restart_of_a_superseded_migration_is_declined() {
    let temp = TempDir::new().expect("tempdir");
    // The owner's row names its own replacement, while the evidence still
    // records that the observed launch belonged to this record before it moved.
    let owner_replacement =
        persisted_row(&owned_row_id("session-a"), Some("session-a"), WORKER_AGENT, "exec-owner-replacement", true);
    persist_background_ledger(
        temp.path(),
        vec![owner_replacement],
        &[MigratedBackgroundLaunch {
            base_id: WORKER.to_string(),
            exec_session_id: OBSERVED_EXEC.to_string(),
        }],
    )
    .await
    .expect("seed the migrated launch and its owner's replacement");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-restart", Some(OBSERVED_EXEC)))
        .await
        .expect("a superseded migration is declined, not an error");

    assert!(
        matches!(reservation, BackgroundRunReservation::Declined),
        "a restart must not claim a launch whose migration the owner has already replaced"
    );
    let persisted = load_background_state(temp.path()).await.expect("read the ledger");
    assert!(
        persisted.records.iter().all(|record| record.exec_session_id != "exec-restart"),
        "a declined reservation must not write a replacement claim"
    );

    controller.signal_shutdown().await;
}
