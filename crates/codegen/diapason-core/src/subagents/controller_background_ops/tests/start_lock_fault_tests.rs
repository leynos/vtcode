//! Pins that an explicit start cannot reserve without reading the ledger.
//!
//! Declared by `tests.rs`. The reservation writes its claim inside the ledger's
//! file lock, and that lock also covers the read of any row another process
//! wrote. A start that fell back to a memory-only claim on a contended ledger
//! never performed that read, so it could spawn beside a launch the ledger
//! already named.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use crate::subagents::background::{load_background_state, lock_background_state, persist_background_state};

use super::super::{BackgroundRunRequest, BackgroundRunReservation};
use super::reserve_support::{
    PERSISTED_STOPPED_STATUS, WORKER, WORKER_AGENT, persisted_row, persisted_row_with_status, plan, worker_spec,
};
use super::support::background_test_controller;

/// An explicit start whose ledger cannot be locked is refused, not downgraded.
///
/// A contended ledger is not evidence that no other launch exists. The start
/// used to fall back to a claim that existed only in memory, which spawned a
/// process beside the row the ledger already held. The fault must reach the
/// caller and the reservation must leave the record untouched, so a retry can
/// read the winning claim once the holder releases the lock.
#[tokio::test]
async fn an_explicit_start_whose_ledger_cannot_be_locked_is_refused() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    // A directory at the lock path makes acquiring it fail outright, which is
    // the same refusal a contended ledger eventually reports.
    let lock_path = crate::subagents::background::background_state_path_for_tests(temp.path())
        .with_file_name("background_subagents.lock");
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks acquisition");

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: WORKER_AGENT.to_string(),
            stable_id: Some(WORKER.to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect_err("a start that cannot read the ledger must not reserve a launch");

    assert!(
        format!("{error:#}").contains("background_subagents.lock"),
        "the refusal names the ledger lock it could not take: {error:#}"
    );
    let state = controller.state.read().await;
    assert!(
        !state.background_children.contains_key(WORKER),
        "a start that never read the ledger must not claim the record"
    );
    drop(state);
    assert!(
        load_background_state(temp.path())
            .await
            .expect("read the ledger")
            .records
            .is_empty(),
        "the refused start must not write a claim"
    );

    std::fs::remove_dir(&lock_path).expect("the lock path is usable again");
    controller.signal_shutdown().await;
}

/// The reservation itself refuses a claim whose ledger cannot be read.
///
/// The public entrypoint above reports the same fault, but its pre-check takes
/// the same lock first, so that case can pass without the reservation ever
/// running. This one drives the reservation directly: the claim and the read it
/// acts on are one durable transition, so a ledger that cannot be locked must
/// leave both memory and the ledger untouched.
#[tokio::test]
async fn a_reservation_whose_ledger_cannot_be_locked_writes_nothing() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let lock_path = crate::subagents::background::background_state_path_for_tests(temp.path())
        .with_file_name("background_subagents.lock");
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks acquisition");

    let error = controller
        .reserve_background_run(&plan(worker_spec(), "exec-direct", None))
        .await
        .expect_err("a reservation that cannot read the ledger must not claim the record");
    assert!(
        format!("{error:#}").contains("background_subagents.lock"),
        "the refusal names the ledger lock it could not take: {error:#}"
    );
    let state = controller.state.read().await;
    assert!(
        !state.background_children.contains_key(WORKER),
        "a reservation that never read the ledger must not claim the record"
    );
    drop(state);
    assert!(
        load_background_state(temp.path())
            .await
            .expect("read the ledger")
            .records
            .is_empty(),
        "the refused reservation must not write a claim"
    );

    std::fs::remove_dir(&lock_path).expect("the lock path is usable again");
    controller.signal_shutdown().await;
}

/// After the contention clears, a retry inspects the winning claim.
///
/// This is the half of the rule the refusal must preserve: propagating the
/// fault is only useful if the caller can then retry and be told about the
/// launch that won. The winning claim is seeded as the ledger row another
/// controller would have written, and the retry must decline over it rather
/// than replace it.
#[tokio::test]
async fn a_retry_after_the_contention_clears_inspects_the_winning_claim() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let lock_path = crate::subagents::background::background_state_path_for_tests(temp.path())
        .with_file_name("background_subagents.lock");
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks acquisition");
    controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: WORKER_AGENT.to_string(),
            stable_id: Some(WORKER.to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect_err("the blocked start is refused");
    std::fs::remove_dir(&lock_path).expect("the lock path is usable again");

    // The other controller's claim reaches the ledger by the time the retry runs.
    persist_background_state(temp.path(), vec![persisted_row(WORKER, None, WORKER_AGENT, "exec-winner", true)])
        .await
        .expect("seed the winning claim");

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: WORKER_AGENT.to_string(),
            stable_id: Some(WORKER.to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect_err("the retry must decline the winning claim");
    assert!(
        format!("{error:#}").contains("claimed by another controller"),
        "the retry is told the launch is already claimed: {error:#}"
    );
    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some("exec-winner"),
        "the retry must leave the winning claim in place"
    );
    assert!(
        !controller
            .state
            .read()
            .await
            .background_children
            .get(WORKER)
            .is_some_and(|record| record.status == BackgroundSubprocessStatus::Starting),
        "the retry must not reserve a second launch"
    );

    controller.signal_shutdown().await;
}

/// Acquiring the ledger lock is what the start is refused on.
///
/// The positive control for the refusal above: with the lock path usable, the
/// same acquisition succeeds, so the failure the other cases observe really is
/// the lock and not an unrelated path problem.
#[tokio::test]
async fn the_ledger_lock_is_acquirable_when_its_path_is_usable() {
    let temp = TempDir::new().expect("tempdir");
    let guard = lock_background_state(temp.path())
        .await
        .expect("an unblocked lock path acquires");
    drop(guard);
}

/// A confirmed spawn failure does not block the next explicit start.
///
/// A settlement writes `Error` only after the spawn was refused or the process
/// was observed to exit, so the row names no live process. Treating it as a
/// competing claim left a task that failed permanently unstartable: every
/// later request was told another controller owned a launch that had never run.
/// The `Error` row is the empty slot an explicit start fills.
#[tokio::test]
async fn a_confirmed_spawn_failure_does_not_block_an_explicit_start() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(
        temp.path(),
        vec![persisted_row_with_status(
            WORKER,
            None,
            WORKER_AGENT,
            "exec-failed",
            true,
            "error",
        )],
    )
    .await
    .expect("seed the confirmed failure");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-retry", None))
        .await
        .expect("a confirmed failure does not claim the record");
    assert!(
        matches!(reservation, BackgroundRunReservation::Reserved),
        "the explicit start claims the record a failed launch left behind"
    );

    controller.signal_shutdown().await;
}

/// A failure whose close is unconfirmed still blocks an explicit start.
///
/// The other half of the rule: a stop whose close faulted leaves an `Error` row
/// naming a process that may still be live. Starting beside it would put two
/// processes behind one task id, so the row must still be refused until the
/// close is retried.
#[tokio::test]
async fn a_failure_whose_close_is_unconfirmed_still_blocks_an_explicit_start() {
    let temp = TempDir::new().expect("tempdir");
    let mut failed =
        persisted_row_with_status(WORKER, None, WORKER_AGENT, "exec-unclosed", false, PERSISTED_STOPPED_STATUS);
    failed.status = BackgroundSubprocessStatus::Error;
    failed.close_fault =
        Some("Failed to close the background subprocess's exec session: close transport fault".to_string());
    persist_background_state(temp.path(), vec![failed])
        .await
        .expect("seed the unconfirmed close");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let error = controller
        .reserve_background_run(&plan(worker_spec(), "exec-retry", None))
        .await
        .expect_err("an unconfirmed close must still refuse the start");
    assert!(
        format!("{error:#}").contains("still owes the close"),
        "the caller is told the previous launch's close is owed: {error:#}"
    );

    controller.signal_shutdown().await;
}

/// A stopped row is still the ordinary case an explicit start replaces.
///
/// The status the fixture now carries must not change the long-standing rule:
/// a cleanly stopped row is the empty slot a start fills.
#[tokio::test]
async fn a_stopped_row_still_allows_an_explicit_start() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(
        temp.path(),
        vec![persisted_row_with_status(
            WORKER,
            None,
            WORKER_AGENT,
            "exec-stopped",
            false,
            PERSISTED_STOPPED_STATUS,
        )],
    )
    .await
    .expect("seed the stopped row");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", None))
        .await
        .expect("a stopped row does not block a start");
    assert!(matches!(reservation, BackgroundRunReservation::Reserved));

    controller.signal_shutdown().await;
}
