//! Deterministic reconciliation of a controller snapshot with the ledger.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Pins order independence,
//! cancellation dominance and restart freshness; ownership precedence and the
//! per-canonical-id removal authority live in `ownership_supersession_tests.rs`.
//! It builds its inputs with the helpers in `merge_oracle_tests.rs`.
use super::super::*;
use super::merge_oracle_tests::{merge_launch_record, merge_owner_record, merge_state};

/// Reads the single record that survives reconciliation of one canonical id.
fn merged_state(records: impl IntoIterator<Item = PersistedBackgroundRecord>) -> (String, bool, String) {
    let merged = records.into_iter().collect::<Vec<_>>();
    assert_eq!(merged.len(), 1, "the fixtures describe one canonical record");
    merge_state(merged.first().expect("the merged record"))
}

/// Every input order must select the same copy of a canonical record.
///
/// A flat comparator over every copy of one id is cyclic: cancellation
/// dominance says the stopped `exec-1` copy outranks the newer active copy of
/// that launch, while timestamp order says the newer `exec-2` launch outranks
/// the stopped one. The same three records therefore selected a different
/// launch depending on file order, and one order resurrected the cancelled
/// launch. Grouping by launch before comparing freshness must make the load
/// collapse and the save merge agree in every order.
#[test]
fn reconciliation_is_order_independent_for_a_cancelled_and_restarted_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let mut active_exec_1 = merge_launch_record(0, 3, true, "exec-1", "active-exec-1");
    active_exec_1.started_at = Some("2026-08-28T00:00:00Z".parse().expect("exec-1 launch time"));
    let mut cancelled_exec_1 = merge_launch_record(0, 1, false, "exec-1", "cancelled-exec-1");
    cancelled_exec_1.started_at = Some("2026-08-28T00:00:00Z".parse().expect("exec-1 launch time"));
    let mut active_exec_2 = merge_launch_record(0, 2, true, "exec-2", "active-exec-2");
    active_exec_2.started_at = Some("2026-08-28T00:00:02Z".parse().expect("exec-2 launch time"));
    let records = [active_exec_1, cancelled_exec_1, active_exec_2];
    let expected = ("active-exec-2".to_owned(), true, "running".to_owned());

    for order in [[0_usize, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let ordered = order.iter().map(|index| records[*index].clone()).collect::<Vec<_>>();
        assert_eq!(
            merged_state(collect_by_canonical_id(ordered.clone()).into_values()),
            expected,
            "the load collapse must select the restarted launch for order {order:?}"
        );
        let merged = merge_background_records(
            ordered.clone(),
            Vec::new(),
            Some("session-a"),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &[],
        );
        assert_eq!(
            merged_state(merged),
            expected,
            "the save merge must select the restarted launch for order {order:?}"
        );
        // The trio also arrives split across the persisted and snapshot sides.
        let mut persisted = ordered;
        let snapshot = persisted.split_off(1);
        let merged =
            merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);
        assert_eq!(
            merged_state(merged),
            expected,
            "the save merge must select the restarted launch when the order splits the trio"
        );
    }
}

/// A newer restart supersedes the launch it replaced in every input order.
///
/// Within one exec session a cancellation is absolute, but a restart names a
/// new session, so the two per-launch survivors are compared by `updated_at`.
/// Reversing the input must not let the older stopped launch win.
#[test]
fn restart_reconciliation_is_order_independent() {
    // Shape 0 is `worker` owned by `session-a`.
    let records = [
        merge_launch_record(0, 2, false, "exec-1", "stopped"),
        merge_launch_record(0, 3, true, "exec-2", "restarted"),
    ];
    let expected = ("restarted".to_owned(), true, "running".to_owned());

    for order in [[0_usize, 1], [1, 0]] {
        let ordered = order.iter().map(|index| records[*index].clone()).collect::<Vec<_>>();
        assert_eq!(
            merged_state(collect_by_canonical_id(ordered.clone()).into_values()),
            expected,
            "the load collapse must keep the newer restart for order {order:?}"
        );
        let merged = merge_background_records(
            ordered.clone(),
            Vec::new(),
            Some("session-a"),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &[],
        );
        assert_eq!(merged_state(merged), expected, "the save merge must keep the newer restart for order {order:?}");
        let mut persisted = ordered;
        let snapshot = persisted.split_off(1);
        let merged =
            merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);
        assert_eq!(merged_state(merged), expected, "the restart must win whichever side carries it");
    }
}

/// An owner-scoped save must not undo a cancellation another controller wrote.
///
/// An unscoped controller can persist a stop first; the owner then saves the
/// stale active copy it holds. The newer persisted stop must win, because
/// restore selection reads `desired_enabled`.
#[test]
fn owner_merge_keeps_a_newer_cancellation() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_owner_record(0, 2, false, "cancelled")];
    let snapshot = vec![merge_owner_record(0, 1, true, "stale-active")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merge_state(&merged[0]), ("cancelled".to_owned(), false, "stopped".to_owned()));
}

/// A stop outranks a later refresh of the same launch.
///
/// A refresh advances `updated_at` without changing intent, so timestamp
/// order alone would resurrect the stopped task; the stop wins even when it
/// is older.
#[test]
fn owner_merge_keeps_a_cancellation_a_refresh_outdates() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 1, false, "exec-launch-1", "cancelled")];
    let snapshot = vec![merge_launch_record(0, 2, true, "exec-launch-1", "refreshed")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]),
        ("cancelled".to_owned(), false, "stopped".to_owned()),
        "a refresh of the stopped launch must not undo the cancellation"
    );
}

/// A snapshot cancellation outranks a newer persisted refresh of the launch.
///
/// The stop can sit on the snapshot side too, so precedence must be checked
/// in both directions; otherwise a newer persisted refresh would undo a
/// cancellation this controller just wrote.
#[test]
fn owner_merge_keeps_a_snapshot_cancellation_over_a_newer_persisted_refresh() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 3, true, "exec-launch-1", "persisted-refresh")];
    let snapshot = vec![merge_launch_record(0, 2, false, "exec-launch-1", "snapshot-cancelled")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]),
        ("snapshot-cancelled".to_owned(), false, "stopped".to_owned()),
        "a newer persisted refresh must not undo the cancellation in the snapshot"
    );
}

/// A restart replaces the launch, so its snapshot is not a stale copy.
///
/// The persisted stop names the session that was stopped, while the snapshot
/// names a replacement launch the owner has since written.
#[test]
fn owner_merge_accepts_a_restart_with_a_new_exec_session() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 2, false, "exec-launch-1", "cancelled")];
    let snapshot = vec![merge_launch_record(0, 3, true, "exec-launch-2", "restarted")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]),
        ("restarted".to_owned(), true, "running".to_owned()),
        "a restart names a new exec session and is not the stale copy a stop outranks"
    );
}

/// Equal copies of one launch must resolve the same way in either order.
///
/// Two copies of one launch that agree on intent and carry the same
/// `updated_at` differ only in the status they report. Arrival order alone used
/// to decide which one a load reported, so reversing the two rows changed the
/// record a caller saw. The settled copy is the outcome of the launch, so it
/// must win whichever row the ledger lists first.
#[test]
fn equal_launch_copies_resolve_the_same_way_in_either_order() {
    // Shape 0 is `worker` owned by `session-a`; both copies name one launch, one
    // timestamp and the same intent, and differ only in the status they report.
    let running = merge_launch_record(0, 2, true, "exec-tied", "running-copy");
    let mut stopped = merge_launch_record(0, 2, true, "exec-tied", "settled-copy");
    stopped.status = crate::subagents::BackgroundSubprocessStatus::Stopped;
    stopped.ended_at = Some("2026-08-28T00:00:09Z".parse().expect("the stop instant"));

    let expected = ("settled-copy".to_owned(), true, "stopped".to_owned());
    for records in [
        vec![running.clone(), stopped.clone()],
        vec![stopped.clone(), running.clone()],
    ] {
        assert_eq!(
            merged_state(collect_by_canonical_id(records.clone()).into_values()),
            expected,
            "the load collapse must keep the settled copy whichever row comes first: {records:?}"
        );
        let merged = merge_background_records(
            records.clone(),
            Vec::new(),
            Some("session-a"),
            &BTreeSet::new(),
            &BTreeMap::new(),
            &[],
        );
        assert_eq!(
            merged_state(merged),
            expected,
            "the save merge must keep the settled copy whichever row comes first"
        );
    }
}

/// Same-launch copies that tie on freshness resolve by the phase they record.
///
/// `Running` and `Error`, or `Starting` and `Running`, can arrive with one
/// `updated_at` and the same intent. Arrival order alone must not decide which
/// status a load restores, so the later phase wins whichever row is listed
/// first. A recorded failure outranks a bare stop for the same reason the
/// settlement path never relabels one as the other.
#[test]
fn equal_launch_copies_tie_break_on_the_recorded_phase() {
    use crate::subagents::BackgroundSubprocessStatus;

    let mut running = merge_launch_record(0, 2, true, "exec-tied", "running-copy");
    running.status = BackgroundSubprocessStatus::Running;
    let mut failed = merge_launch_record(0, 2, true, "exec-tied", "failed-copy");
    failed.status = BackgroundSubprocessStatus::Error;
    for records in [
        vec![running.clone(), failed.clone()],
        vec![failed.clone(), running.clone()],
    ] {
        assert_eq!(
            merged_state(collect_by_canonical_id(records).into_values()).0,
            "failed-copy",
            "a recorded failure outranks an active copy of the same launch"
        );
    }

    let mut starting = merge_launch_record(0, 2, true, "exec-tied", "starting-copy");
    starting.status = BackgroundSubprocessStatus::Starting;
    let mut later_phase = merge_launch_record(0, 2, true, "exec-tied", "running-copy");
    later_phase.status = BackgroundSubprocessStatus::Running;
    for records in [
        vec![starting.clone(), later_phase.clone()],
        vec![later_phase.clone(), starting.clone()],
    ] {
        assert_eq!(
            merged_state(collect_by_canonical_id(records).into_values()).0,
            "running-copy",
            "the later active phase outranks the claim"
        );
    }
}

/// A claim without a recorded start cannot displace a persisted launch.
///
/// A reservation writes `Starting` with an exec session and no `started_at`.
/// Its `launch_moment` is then only the moment the claim was written, which is
/// not comparable with a persisted launch's `started_at`: a claim could look
/// newer than a replacement persisted after it merely because the
/// replacement's process began earlier. Recorded start evidence is required
/// before a snapshot copy may supersede a different persisted launch.
#[test]
fn a_claim_without_a_recorded_start_does_not_displace_a_persisted_launch() {
    use crate::subagents::BackgroundSubprocessStatus;

    let mut persisted = merge_launch_record(0, 1, true, "exec-persisted", "persisted-replacement");
    persisted.started_at = Some("2026-08-28T00:00:00Z".parse().expect("launch time"));
    let mut claim = merge_launch_record(0, 3, true, "exec-claim", "snapshot-claim");
    claim.status = BackgroundSubprocessStatus::Starting;
    claim.started_at = None;

    let merged = merge_background_records(
        vec![persisted],
        vec![claim],
        Some("session-a"),
        &BTreeSet::new(),
        &BTreeMap::new(),
        &[],
    );

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]).0,
        "persisted-replacement",
        "a persisted launch that recorded its start survives a newer claim"
    );
}

/// A launch with recorded start evidence still supersedes the one it replaced.
///
/// The control for the rule above: ordering by an explicit `started_at` keeps
/// its existing behaviour, so a real restart is not blocked by the claim rule.
#[test]
fn a_snapshot_launch_with_a_recorded_start_supersedes_a_persisted_launch() {
    let mut persisted = merge_launch_record(0, 1, true, "exec-persisted", "persisted-old");
    persisted.started_at = Some("2026-08-28T00:00:00Z".parse().expect("launch time"));
    let mut replacing = merge_launch_record(0, 3, true, "exec-replacing", "snapshot-restart");
    replacing.started_at = Some("2026-08-28T00:00:03Z".parse().expect("launch time"));

    let merged = merge_background_records(
        vec![persisted],
        vec![replacing],
        Some("session-a"),
        &BTreeSet::new(),
        &BTreeMap::new(),
        &[],
    );

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]).0,
        "snapshot-restart",
        "a real restart still supersedes the launch it replaced"
    );
}
