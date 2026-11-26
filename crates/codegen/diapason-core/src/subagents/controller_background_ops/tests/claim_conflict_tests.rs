//! Regressions for the claims an explicit start and a bound replacement refuse.
//!
//! Declared by `tests.rs`. Both cases turn on the ledger row the reservation
//! reads under the lock it writes its own claim with, which is the only place
//! two controllers' claims can be ordered.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use crate::subagents::PersistedBackgroundRecord;
use crate::subagents::SubagentInvocationContext;
use crate::subagents::background::{load_background_state, persist_background_state};
#[cfg(unix)]
use crate::subagents::controller_persistence::ledger_write_checkpoints;
use crate::subagents::with_subagent_invocation_context;

use super::super::BackgroundRunReservation;
use super::super::ExplicitStartAdmission;
use super::super::controller_background_cancel::mark_background_stop_close_pending;
#[cfg(unix)]
use super::super::controller_background_lifecycle::background_launch_is_owed;
use super::super::controller_background_settlement::pending_background_ledger_write_generation;
use super::super::explicit_start_admission;
use super::reserve_support::{
    OBSERVED_EXEC, PERSISTED_RUNNING_STATUS, PERSISTED_STOPPED_STATUS, WORKER, WORKER_AGENT, insert_observed_record,
    persisted_row, persisted_row_launched_at, persisted_row_with_status, plan, worker_spec,
};
use super::support::{RecordingObserver, background_test_controller};

/// An explicit start cannot reserve over a claim another controller wrote.
///
/// Both controllers passed the explicit-start pre-check while the ledger was
/// empty, so neither saw the other. The first writes its claim under the ledger
/// lock; the second's reservation then reads the same row under that same lock,
/// and a start that replaced it would put two processes beside one recorded
/// launch. The claim is decided in the reservation, not only in the pre-check,
/// because only the reservation holds the lock it writes with.
#[tokio::test]
async fn an_explicit_start_declines_a_claim_written_after_its_pre_check() {
    let temp = TempDir::new().expect("tempdir");
    let first = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let second = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let claimed = first
        .reserve_background_run(&plan(worker_spec(), "exec-first", None))
        .await
        .expect("the first reservation claims the record");
    assert!(
        matches!(&claimed, BackgroundRunReservation::Reserved),
        "the first start owns a record the ledger did not hold"
    );

    let error = second
        .reserve_background_run(&plan(worker_spec(), "exec-second", None))
        .await
        .expect_err("the second start must not replace a claim another controller wrote");
    assert!(
        format!("{error:#}").contains("claimed by another controller"),
        "the caller is told the launch is already claimed: {error:#}"
    );
    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some("exec-first"),
        "the second start must not overwrite the persisted claim"
    );
    assert!(
        !second.state.read().await.background_children.contains_key(WORKER),
        "a refused start inserts no record"
    );

    first.signal_shutdown().await;
    second.signal_shutdown().await;
}

/// An explicit start still replaces a record the ledger holds stopped.
///
/// A stopped row is not a claim: an explicit start is how a stopped task is
/// started again, so the same lock that refuses a live claim must let this one
/// through.
#[tokio::test]
async fn an_explicit_start_replaces_a_record_the_ledger_holds_stopped() {
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
    .expect("seed the stopped record");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", None))
        .await
        .expect("a stopped record does not block an explicit start");
    assert!(
        matches!(&reservation, BackgroundRunReservation::Reserved),
        "the explicit start claims the stopped record"
    );
    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some("exec-replacement"),
        "the start's claim replaces the stopped row"
    );

    controller.signal_shutdown().await;
}

/// An explicit start still reuses the launch the ledger holds for its own record.
///
/// The persisted row names the exec session this controller's own record names,
/// so it is this launch's copy rather than a competing claim; reuse must answer
/// with it instead of refusing the request.
#[tokio::test]
async fn an_explicit_start_reuses_the_launch_the_ledger_holds_for_its_own_record() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![persisted_row(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, true)])
        .await
        .expect("seed this controller's own launch");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", None))
        .await
        .expect("the controller's own launch is reuse, not a competing claim");
    match reservation {
        BackgroundRunReservation::Reused(entry) => {
            assert_eq!(entry.exec_session_id, OBSERVED_EXEC, "reuse answers for the launch the ledger holds");
        }
        other => panic!("the controller's own launch must be reused, got {other:?}"),
    }

    controller.signal_shutdown().await;
}

/// A reuse cannot answer for a launch another parent session drives.
///
/// The pre-lock reuse check compares the ambient invocation context, which no
/// caller outside a tool invocation has. The resolved parent is available on the
/// reservation's plan, so the rule is applied to it as well: answering with the
/// existing launch would route its progress to the old parent while the caller
/// is told its own request succeeded.
#[tokio::test]
async fn a_reuse_for_another_parent_session_is_refused_without_an_invocation_context() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut(WORKER).expect("the observed record");
        record.parent_session_id = "other-parent".to_string();
    }

    let error = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", None))
        .await
        .expect_err("a launch another parent drives must not be reused");
    assert!(
        format!("{error:#}").contains("already active"),
        "the refusal uses the active-launch rule: {error:#}"
    );

    controller.signal_shutdown().await;
}

/// A bound replacement must confirm the ledger kept the claim it wrote.
///
/// The save merges rather than replaces, and a persisted launch that recorded
/// its start outranks a claim that has not begun, so the ledger still names the
/// previous launch. Reporting `Reserved` then would start a process beside a
/// launch the ledger names. The persisted start is seeded ahead of the wall
/// clock so the fixture also covers the clock skew between writers that first
/// exposed this; the refusal must not depend on that ordering.
#[tokio::test]
async fn a_bound_replacement_the_ledger_did_not_record_is_refused() {
    let temp = TempDir::new().expect("tempdir");
    // Derived from the wall clock: a fixed date would stop exercising the merge
    // once that date had passed.
    let skewed_start = (chrono::Utc::now() + chrono::Duration::days(3_650)).to_rfc3339();
    persist_background_state(
        temp.path(),
        vec![persisted_row_launched_at(
            WORKER,
            None,
            WORKER_AGENT,
            OBSERVED_EXEC,
            true,
            Some(skewed_start.as_str()),
        )],
    )
    .await
    .expect("seed the persisted launch ahead of the wall clock");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;

    let error = controller
        .reserve_background_run(&plan(worker_spec(), "exec-replacement", Some(OBSERVED_EXEC)))
        .await
        .expect_err("a claim the ledger did not keep must refuse the replacement");
    assert!(
        format!("{error:#}").contains("did not keep the claim"),
        "the caller is told the ledger did not record the claim: {error:#}"
    );
    let ledger = load_background_state(temp.path()).await.expect("read the ledger");
    assert_eq!(
        ledger.records.first().map(|record| record.exec_session_id.as_str()),
        Some(OBSERVED_EXEC),
        "the ledger must still name the launch it kept, with no second launch claimed"
    );
    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Error,
        "a refused replacement settles the launch it never started"
    );
    drop(state);

    controller.signal_shutdown().await;
}

/// A reservation dropped inside its first await settles the claim it wrote.
///
/// The claim is written under the state lock and the reservation then awaits its
/// durable write and its publication. The launch arms its own guard only once
/// `Reserved` is returned, so a caller dropped in that window would leave a
/// `Starting` record whose non-retryable launch obligation nothing drives. The
/// reservation therefore arms the same guard at the claim write; dropping the
/// future at the claim write must settle the record instead of leaving it
/// `Starting` forever. The ledger-write checkpoint parks the save deterministically.
#[cfg(unix)]
#[tokio::test]
async fn a_reservation_dropped_during_its_claim_write_settles_the_claim_it_wrote() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let (entered, _release) = ledger_write_checkpoints::arm(temp.path());
    {
        let claimed_plan = plan(worker_spec(), "exec-claimed", None);
        let reserve = controller.reserve_background_run(&claimed_plan);
        tokio::pin!(reserve);
        tokio::select! {
            result = &mut reserve => {
                panic!("the reservation answered before its claim write was parked: {result:?}")
            }
            () = entered.notified() => {}
        }
        let state = controller.state.read().await;
        let record = state
            .background_children
            .get(WORKER)
            .expect("the claim is written before its save");
        assert_eq!(record.exec_session_id, "exec-claimed", "the claim names the launch this reservation wrote");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "the claim exists in memory while its durable write is in flight"
        );
    }

    // The future was dropped inside the parked write; the reservation's guard
    // must have settled the claim it wrote.
    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("the claim leaves a record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Error,
            "a claim abandoned before its spawn must not report an active launch"
        );
        assert!(
            record
                .error
                .as_deref()
                .is_some_and(|error| error.contains("abandoned before its process started")),
            "{:?}",
            record.error
        );
        assert!(record.ended_at.is_some(), "a settled claim has ended");
    }
    assert!(
        !background_launch_is_owed(&controller.state, WORKER, "exec-claimed"),
        "the abandoned claim's obligation is cleared so refresh can settle it"
    );
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the settlement owes the ledger the failure it recorded"
    );

    controller.signal_shutdown().await;
}

/// Reuse serves only the parent session the launch belongs to.
///
/// Reuse returns the existing launch unchanged, so the parent this request
/// resolved is dropped on that path and progress keeps routing to the record's
/// parent. The rule therefore compares the resolved parent for every caller, not
/// only for callers inside a tool invocation that names one.
#[rstest::rstest]
#[case::same_parent("parent-session", "parent-session", false)]
#[case::other_parent("other-session", "parent-session", true)]
#[case::untracked_record("", "parent-session", false)]
#[case::untracked_request("parent-session", "", true)]
fn a_reuse_serves_only_the_parent_it_belongs_to(
    #[case] record_parent: &str,
    #[case] requested_parent: &str,
    #[case] expected: bool,
) {
    assert_eq!(
        super::super::reservation::reuse_is_for_another_parent(record_parent, requested_parent),
        expected,
        "reuse must be refused exactly when the launch belongs to another parent"
    );
}

/// Reuse rebinds a parentless record onto the session that asked for it.
///
/// A legacy record names no parent, so nothing routes its progress and the
/// observer path publishes nothing for an empty parent. The pre-lock selector
/// cannot rebind it, so reuse happens in the reservation, which holds the write
/// lock: the record moves onto the requesting session and its lifecycle updates
/// reach that session's observers. The rebind is owed to the ledger, because the
/// routing rule must survive a restart.
#[tokio::test]
async fn a_reuse_rebinds_a_parentless_record_onto_the_requesting_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut(WORKER).expect("the observed launch");
        record.parent_session_id = String::new();
    }
    let observer = std::sync::Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("parent-session", observer.clone());

    let reservation = controller
        .reserve_background_run(&plan(worker_spec(), OBSERVED_EXEC, None))
        .await
        .expect("the parentless launch is reused");
    assert!(matches!(reservation, BackgroundRunReservation::Reused(_)));

    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(
            record.parent_session_id, "parent-session",
            "reuse moves a legacy record onto the session that asked for it"
        );
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the rebind must reach the ledger through the owed write"
    );
    assert!(
        controller
            .retry_owed_background_ledger_write()
            .await
            .expect("the rebind's owed write succeeds"),
        "the rebind is what the owed write persists"
    );
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the rebound record reaches the ledger");
    assert_eq!(
        stored.parent_session_id, "parent-session",
        "the durable row must keep the parent the rebind gave it, or a restart loses the route"
    );

    controller.publish_background_status(WORKER).await;
    assert!(
        !observer.background_entries().is_empty(),
        "the requesting session now receives the launch's lifecycle updates"
    );

    drop(registration);
    controller.signal_shutdown().await;
}

/// A parentless record is reused and rebound for a caller inside a tool invocation.
///
/// The ambient invocation context names the session the tool call belongs to. A
/// record that names no parent predates parent tracking, so it has no owner to
/// conflict with: comparing it against that context refused the reuse with the
/// "already active" error, and the rebind that exists for exactly such a record
/// could never run for a tool caller. The reuse must succeed and take the record
/// onto the invoking session.
#[tokio::test]
async fn a_parentless_reuse_rebinds_inside_a_tool_invocation() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    {
        let mut state = controller.state.write().await;
        let record = state.background_children.get_mut(WORKER).expect("the observed launch");
        record.parent_session_id = String::new();
    }
    let context = SubagentInvocationContext::new("parent-session".to_string(), Vec::new(), String::new());

    let reservation = with_subagent_invocation_context(
        context,
        controller.reserve_background_run(&plan(worker_spec(), OBSERVED_EXEC, None)),
    )
    .await
    .expect("a tool invocation may reuse a parentless record");

    assert!(matches!(reservation, BackgroundRunReservation::Reused(_)));
    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.parent_session_id, "parent-session",
        "the reuse takes the parentless record onto the invoking session"
    );
    drop(state);

    controller.signal_shutdown().await;
}

/// A claim dropped under contention is settled by the next refresh.
///
/// The claim guard settles its record synchronously, so it cannot await the
/// state lock. When another task holds that lock the status is left as it was,
/// and the guard still clears the launch obligation: a `Starting` record that
/// owes no launch is settled by the next refresh instead of exempting itself
/// from settlement forever. The drop test above holds no lock, so this is the
/// only case that reaches the contended branch.
#[cfg(unix)]
#[tokio::test]
async fn a_contended_claim_drop_leaves_the_record_for_refresh_to_settle() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let (entered, _release) = ledger_write_checkpoints::arm(temp.path());
    {
        let claimed_plan = plan(worker_spec(), "exec-contended", None);
        let mut reserve = Box::pin(controller.reserve_background_run(&claimed_plan));
        tokio::select! {
            result = &mut reserve => {
                panic!("the reservation answered before its claim write was parked: {result:?}")
            }
            () = entered.notified() => {}
        }
        // Holding the state lock across the drop is what makes the guard's
        // non-blocking settle fail. The same guard also observes the claim the
        // reservation wrote before it parked.
        let held = controller.state.read().await;
        assert_eq!(
            held.background_children
                .get(WORKER)
                .expect("the claim is written before its save")
                .status,
            BackgroundSubprocessStatus::Starting,
            "the claim exists in memory while its durable write is parked"
        );
        drop(reserve);
        drop(held);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("the claim leaves a record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "a contended drop cannot settle the record, so it leaves it as it was"
        );
    }
    assert!(
        !background_launch_is_owed(&controller.state, WORKER, "exec-contended"),
        "the contended drop still clears the launch obligation"
    );

    controller
        .refresh_background_processes()
        .await
        .expect("the next refresh settles the orphaned claim");

    let state = controller.state.read().await;
    let record = state.background_children.get(WORKER).expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Error,
        "a claim whose driver dropped under contention must be settled by refresh, not left Starting"
    );
    assert!(record.ended_at.is_some(), "a settled claim has ended");
    drop(state);

    controller.signal_shutdown().await;
}

/// An explicit start's admission checks keep their precedence.
///
/// The checks are one decision over the persisted row and this controller's own
/// obligations. An unconfirmed close outranks the exemption for a row naming
/// this controller's own launch, because a stop whose close failed may still
/// have a live process behind it; a desired row naming another launch is a
/// competing claim; and a stop this controller owes declines the start. Each
/// answer is exercised here without writing the ledger or publishing a record.
#[rstest::rstest]
#[case::admitted(None, false, ExplicitStartAdmission::Admitted)]
#[case::unconfirmed_close_outranks_the_own_row(
    Some(persisted_row_with_status(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, false, PERSISTED_RUNNING_STATUS)),
    false,
    ExplicitStartAdmission::CloseUnconfirmed
)]
#[case::competing_claim(
    Some(persisted_row(WORKER, None, WORKER_AGENT, "exec-other", true)),
    false,
    ExplicitStartAdmission::CompetingClaim
)]
#[case::stop_close_pending(None, true, ExplicitStartAdmission::StopClosePending)]
#[tokio::test]
async fn an_explicit_starts_admission_keeps_its_precedence(
    #[case] persisted: Option<PersistedBackgroundRecord>,
    #[case] stop_close_pending: bool,
    #[case] expected: ExplicitStartAdmission,
) {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    insert_observed_record(&controller).await;
    if stop_close_pending {
        mark_background_stop_close_pending(&controller.state, WORKER, OBSERVED_EXEC);
    }

    let state = controller.state.read().await;
    let admission = explicit_start_admission(&controller.state, &state, persisted.as_ref(), WORKER, "exec-replacement");
    assert_eq!(admission, expected, "the admission checks must answer in their documented order");
    drop(state);

    controller.signal_shutdown().await;
}
