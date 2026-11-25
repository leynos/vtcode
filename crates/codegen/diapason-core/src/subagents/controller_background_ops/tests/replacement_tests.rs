//! Pins how the reservation answers for a launch another caller replaced.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use crate::subagents::background::{load_background_state, persist_background_state};

use super::super::BackgroundRunReservation;
use super::super::adopted_stop_gate::gate_next_adopted_background_stop_close;
use super::super::controller_background_cancel::gate_next_background_close;
use super::reserve_support::{
    OBSERVED_EXEC, PERSISTED_STOPPED_STATUS, WORKER, WORKER_AGENT, insert_observed_record, persisted_row,
    persisted_row_with_status, plan, worker_spec,
};
use super::support::{RecordingObserver, background_test_controller};

/// An adopted stop closes the launch it adopted, not a replacement.
///
/// The stop is adopted under the state lock, but the close runs after that
/// lock is released. A concurrent explicit start can take the record in
/// between; re-reading the record then would close the replacement's session
/// and settle a launch that never owed the stop. The gate lands the concurrent
/// start in that window.
#[tokio::test]
async fn an_adopted_stop_closes_the_launch_it_adopted_after_a_concurrent_start() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    // A finished stop, not a close still owed: this case is about the adopted
    // intent reaching the close, not about the obligation an unconfirmed close
    // carries.
    persist_background_state(
        temp.path(),
        vec![persisted_row_with_status(
            WORKER,
            None,
            WORKER_AGENT,
            OBSERVED_EXEC,
            false,
            PERSISTED_STOPPED_STATUS,
        )],
    )
    .await
    .expect("seed the persisted stop");

    let bound_plan = plan(worker_spec(), "exec-replacement", Some(OBSERVED_EXEC));
    let replacement_plan = plan(worker_spec(), "exec-replacement", None);

    let (close_entered, close_release) = gate_next_background_close(&controller.state, OBSERVED_EXEC);
    let (reservation_entered, reservation_release) = gate_next_adopted_background_stop_close(&controller.state);
    let reserve = controller.reserve_background_run(&bound_plan);
    tokio::pin!(reserve);
    tokio::select! {
        result = &mut reserve => {
            panic!("the adopted stop closed before the concurrent start landed: {result:?}")
        },
        () = reservation_entered.notified() => {}
    }

    // A concurrent explicit start takes the record now that the adopted stop
    // has cleared its intent, and names a different exec session.
    let concurrent = controller
        .reserve_background_run(&replacement_plan)
        .await
        .expect("the replacement launch reserves the record");
    assert!(
        matches!(concurrent, BackgroundRunReservation::Reserved),
        "the explicit start claims the record after the adopted stop cleared it"
    );

    reservation_release.notify_one();
    // The reservation future is only polled while something awaits it, so
    // drive it here: the close gate can only be reached from inside that
    // future, and waiting on the gate alone would wait on a future nobody
    // polls.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::select! {
            () = close_entered.notified() => {}
            result = &mut reserve => {
                panic!("the adopted stop answered before it closed its launch: {result:?}")
            }
        }
    })
    .await
    .expect("the adopted stop must close the launch whose stop it adopted");
    close_release.notify_one();

    let reservation = reserve.await.expect("the adopted stop still answers the caller");
    assert!(matches!(reservation, BackgroundRunReservation::Declined));

    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(record.exec_session_id, "exec-replacement", "the replacement launch keeps the record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Starting,
        "the adopted stop's close must not settle the replacement launch"
    );
    assert!(record.desired_enabled, "the replacement launch is still wanted");
    drop(state);

    controller.signal_shutdown().await;
}

/// A claimed record is reported to its observers before the call answers.
#[tokio::test]
async fn a_reserved_claim_is_published() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let observer = std::sync::Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-published", None))
        .await
        .expect("the unbound plan claims the record");
    assert!(matches!(reservation, BackgroundRunReservation::Reserved));

    let published = observer.background_entries();
    assert_eq!(
        published.last().map(|entry| entry.status),
        Some(BackgroundSubprocessStatus::Starting),
        "the fresh claim must reach observers: {published:?}"
    );

    drop(registration);
    controller.signal_shutdown().await;
}

/// The unbound reservation still reports a record it already runs.
#[tokio::test]
async fn an_active_record_with_matching_settings_is_reused() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", None))
        .await
        .expect("a matching active record is reuse, not an error");

    match reservation {
        BackgroundRunReservation::Reused(entry) => {
            assert_eq!(entry.exec_session_id, OBSERVED_EXEC, "reuse answers for the running launch");
        }
        other => panic!("a matching active record must be reused, got {other:?}"),
    }

    controller.signal_shutdown().await;
}

/// A stop of a replacement before its first save displaces its predecessor.
///
/// The replacement is claimed under a new exec session while the ledger still
/// names the launch it replaced. A stop that lands before the replacement's
/// first save must not leave the predecessor row in place: the merge compares a
/// stop with the session it names, so a predecessor the stop never mentions
/// would survive and a reattach would restore the launch that was replaced.
#[tokio::test]
async fn a_stop_of_a_replacement_displaces_the_launch_it_replaced() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![persisted_row(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, true)])
        .await
        .expect("seed the observed launch");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", Some(OBSERVED_EXEC)))
        .await
        .expect("the bound plan claims the record");
    assert!(matches!(reservation, BackgroundRunReservation::Reserved));

    // The stop lands before the replacement's own launch snapshot is saved.
    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut(WORKER).expect("the claimed record");
        assert_eq!(record.exec_session_id, "exec-replacement", "the claim names the replacement");
        record.desired_enabled = false;
        record.status = BackgroundSubprocessStatus::Stopped;
        record.ended_at = Some(chrono::Utc::now());
        record.updated_at = chrono::Utc::now();
    }
    controller.save_background_state().await.expect("save the stop");

    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(ledger.records.len(), 1, "the ledger keeps one row for the record");
    let stored = &ledger.records[0];
    assert_eq!(
        stored.exec_session_id, "exec-replacement",
        "the stopped row must name the replacement, not the launch it replaced"
    );
    let restored = crate::subagents::types::BackgroundRecord::from_persisted(stored.clone());
    assert!(!restored.desired_enabled, "a reattach must not restore the launch this replacement replaced");
    assert!(
        ledger.records.iter().all(|record| record.exec_session_id != OBSERVED_EXEC),
        "the predecessor row must not survive beside the launch that replaced it"
    );

    controller.signal_shutdown().await;
}

/// A stop that lands while the claim's own write is paused still wins.
///
/// This is the same transition as the test above, moved one step earlier: the
/// reservation writes its claim inside the ledger lock, and the stop is applied
/// while that write is parked. The reservation must be driven to completion:
/// dropping it while the write is parked cancels the write, and the ledger then
/// keeps the predecessor for reasons that have nothing to do with the stop.
///
/// Completing the claim write must already leave the replacement as the only
/// row under the record's canonical id — the claim carries the replacement's
/// exec session and is the launch this snapshot started, so it displaces the
/// predecessor the merge could otherwise keep. The stop then makes its own
/// save, as a cancellation always does, and settles that same launch: the
/// stopped copy names the replacement's exec session, so it wins within the
/// launch and the ledger ends with the stopped replacement alone. The ledger
/// checkpoint is the harness's own seam for pausing a save, so the window is
/// deterministic rather than a race.
#[cfg(unix)]
#[tokio::test]
async fn a_stop_applied_while_the_claim_write_is_paused_leaves_only_the_stopped_replacement() {
    use crate::subagents::controller_persistence::ledger_write_checkpoints;

    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![persisted_row(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, true)])
        .await
        .expect("seed the observed launch");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let (write_entered, write_release) = ledger_write_checkpoints::arm(temp.path());
    let bound_plan = plan(worker_spec(), "exec-replacement", Some(OBSERVED_EXEC));
    let reserve = controller.reserve_background_run(&bound_plan);
    tokio::pin!(reserve);
    tokio::select! {
        result = &mut reserve => {
            panic!("the reservation answered before its claim write was paused: {result:?}")
        }
        () = write_entered.notified() => {}
    }

    // The claim exists in memory while its write is parked. Stopping it now is
    // the transition the ledger must end up representing.
    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut(WORKER).expect("the claimed record");
        assert_eq!(record.exec_session_id, "exec-replacement", "the paused write is the replacement's claim");
        record.desired_enabled = false;
        record.status = BackgroundSubprocessStatus::Stopped;
        record.ended_at = Some(chrono::Utc::now());
        record.updated_at = chrono::Utc::now();
    }
    write_release.notify_one();

    // The parked write is only resumed when the reservation is polled again, so
    // the reservation must run to completion before the claim can be on disk.
    let reservation = reserve.await.expect("the replacement launch reserves the record");
    assert!(matches!(reservation, BackgroundRunReservation::Reserved));

    // A cancellation's own save is what carries the stop to the ledger; the
    // claim write above could only carry the record as it stood when it began.
    controller.save_background_state().await.expect("save the stop");

    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(ledger.records.len(), 1, "the ledger keeps one row for the record");
    let stored = &ledger.records[0];
    assert_eq!(
        stored.exec_session_id, "exec-replacement",
        "the row written after the paused claim must name the stopped replacement"
    );
    assert!(!stored.desired_enabled, "the stop that landed during the write must reach the ledger");
    assert!(
        ledger.records.iter().all(|record| record.exec_session_id != OBSERVED_EXEC),
        "the launch the replacement displaced must not survive beside it"
    );

    controller.signal_shutdown().await;
}
