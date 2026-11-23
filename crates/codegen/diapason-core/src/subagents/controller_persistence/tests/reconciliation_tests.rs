//! Deterministic reconciliation of a controller snapshot with the ledger.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Pins order independence,
//! cancellation dominance, restart freshness, owner precedence and the
//! per-canonical-id removal authority; it builds its inputs with the helpers in
//! `merge_oracle_tests.rs`.
use super::super::*;
use super::merge_oracle_tests::{merge_canonical_id, merge_launch_record, merge_owner_record, merge_state};

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

/// An owned copy of a launch outranks an ownerless copy of it in every order.
///
/// The launch workspace can hold the ownerless copy after a resumed session
/// migrated the row it launched, so the copy can be newer than the migrated
/// record. Ownership must still decide, or the merge writes a second,
/// restorable record beside the record the session owns.
#[test]
fn an_owned_copy_outranks_an_ownerless_copy_of_the_same_launch() {
    // Shape 0 is `worker` owned by `session-a`; shape 2 is the same agent with
    // no owner and a newer `updated_at`.
    let owned = merge_owner_record(0, 1, true, "owned");
    let ownerless = merge_owner_record(2, 3, true, "ownerless");

    for records in [
        vec![owned.clone(), ownerless.clone()],
        vec![ownerless.clone(), owned.clone()],
    ] {
        let merged = collect_by_canonical_id(records);
        assert_eq!(merged.len(), 1, "the load collapse keeps one copy of the launch");
        let record = merged.values().next().expect("the merged record");
        assert_eq!(
            merge_state(record).0,
            "owned",
            "ownership must outrank the newer ownerless copy of the same launch"
        );
        assert_eq!(serde_json::to_value(record).expect("serialize merged record")["owner_session_id"], "session-a");
    }

    // The save merge applies the same rule whichever side carries each copy.
    for (persisted, snapshot) in [
        (vec![owned.clone()], vec![ownerless.clone()]),
        (vec![ownerless.clone()], vec![owned.clone()]),
        (vec![owned.clone(), ownerless.clone()], Vec::new()),
        (Vec::new(), vec![ownerless.clone(), owned.clone()]),
    ] {
        let merged = merge_background_records(persisted, snapshot, None, &BTreeSet::new(), &BTreeMap::new(), &[]);
        assert_eq!(merged.len(), 1, "the ownerless copy must not survive beside the migrated record");
        assert_eq!(merge_state(&merged[0]).0, "owned");
    }
}

/// Ownership decides a launch even when only the ownerless copy is cancelled.
///
/// An ownerless copy in memory is not authoritative: the only scope that could
/// have written a stop for it is one that does not own the launch. The owned
/// copy is what every scope agrees on, so it is kept and the duplicate is
/// dropped rather than letting an ownerless cancellation rename the record.
#[test]
fn ownership_outranks_a_cancelled_ownerless_copy_of_the_same_launch() {
    // Shape 0 is `worker` owned by `session-a`; shape 2 is the same agent with
    // no owner.
    let owned = merge_owner_record(0, 3, true, "owned");
    let ownerless_cancelled = merge_owner_record(2, 4, false, "ownerless-cancelled");

    let persisted = vec![ownerless_cancelled];
    let snapshot = vec![owned];
    let merged = merge_background_records(persisted, snapshot, None, &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merge_state(&merged[0]).0, "owned", "the launch keeps its owned copy");
}

/// An ownerless copy is kept when it names a different launch.
///
/// Ownership decides within one launch, not across a restart: a copy that
/// names another exec session is the replacement launch, not a stale copy of
/// the one the owned record names.
#[test]
fn an_ownerless_copy_of_a_different_launch_is_kept() {
    // Shape 0 is `worker` owned by `session-a`; shape 2 is the same agent with
    // no owner.
    let owned = merge_launch_record(0, 1, true, "exec-1", "owned");
    let ownerless = merge_launch_record(2, 2, true, "exec-2", "ownerless");

    let merged = merge_background_records(vec![owned], vec![ownerless], None, &BTreeSet::new(), &BTreeMap::new(), &[]);
    let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
    tags.sort();

    assert_eq!(
        tags,
        vec!["owned".to_owned(), "ownerless".to_owned()],
        "two launches of one agent must both survive"
    );
}

/// Shadowing an ownerless copy must not touch another owner's record.
///
/// Shapes 0 and 1 are `worker` owned by different sessions; shape 2 is the same
/// agent with no owner. Only the ownerless copy is dropped.
#[test]
fn ownerless_shadowing_leaves_each_owners_copy_alone() {
    let persisted = vec![
        merge_owner_record(0, 1, true, "owner-a"),
        merge_owner_record(1, 2, true, "owner-b"),
        merge_owner_record(2, 3, true, "ownerless"),
    ];

    let merged = merge_background_records(persisted, Vec::new(), None, &BTreeSet::new(), &BTreeMap::new(), &[]);
    let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
    tags.sort();

    assert_eq!(tags, vec!["owner-a".to_owned(), "owner-b".to_owned()]);
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

/// A record the owner no longer tracks is still removed from the ledger.
///
/// The snapshot is authoritative for the owner's remaining records, but only
/// an id in the removable set may be dropped, so a record both sides hold and
/// one this controller never committed survive.
#[test]
fn owner_merge_removes_a_record_the_snapshot_dropped() {
    // Shape 0 is `worker` and shape 3 is `reviewer`, both owned by `session-a`.
    let persisted = vec![
        merge_owner_record(0, 1, true, "dropped"),
        merge_owner_record(3, 1, true, "kept"),
    ];
    let snapshot = vec![merge_owner_record(3, 1, true, "kept-snapshot")];
    let removable = BTreeSet::from([merge_canonical_id(0)]);

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &removable, &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merge_state(&merged[0]).0, "kept-snapshot");
}

/// An owner-scoped save must not delete a record it never committed.
///
/// A second live controller of the same owner can persist after this one
/// loaded. Absence from the snapshot is not authority to remove, or the other
/// controller's task would vanish from the ledger used for recovery.
#[test]
fn owner_merge_keeps_an_owner_record_it_never_committed() {
    // Shape 0 is `worker` and shape 3 is `reviewer`, both owned by `session-a`.
    let persisted = vec![
        merge_owner_record(0, 1, true, "loaded"),
        merge_owner_record(3, 1, true, "other-controller"),
    ];
    let snapshot = vec![merge_owner_record(0, 1, true, "loaded")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);
    let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
    tags.sort();

    assert_eq!(tags, vec!["loaded".to_owned(), "other-controller".to_owned()]);
}

/// Removal authority is per canonical id, not per launch.
///
/// A scoped controller's snapshot never omits an id it has already committed:
/// its `background_children` only gains entries in production, so a newer
/// launch another controller writes under that id is still joined against a
/// snapshot that carries the id and survives. This test pins the merge's
/// boundary rather than a reachable path. For an id inside the removable set
/// and absent from the snapshot the merge removes whatever launch the ledger
/// now holds, even a newer one; for an id outside the set it removes nothing.
#[test]
fn removal_authority_is_per_canonical_id_not_per_launch() {
    // Shape 0 is `worker` and shape 3 is `reviewer`, both owned by `session-a`.
    // The ledger holds a newer launch under each id than this controller saw.
    let persisted = vec![
        merge_launch_record(0, 2, true, "exec-replacement", "newer-launch-under-committed-id"),
        merge_launch_record(3, 2, true, "exec-reviewer", "newer-launch-under-uncommitted-id"),
    ];
    let removable = BTreeSet::from([merge_canonical_id(0)]);

    let merged = merge_background_records(persisted, Vec::new(), Some("session-a"), &removable, &BTreeMap::new(), &[]);

    assert_eq!(
        merged.len(),
        1,
        "a committed id the snapshot omits is removed even when a newer launch sits under it"
    );
    assert_eq!(
        merge_state(&merged[0]).0,
        "newer-launch-under-uncommitted-id",
        "an id outside the removable set is never removed, so another controller's newer launch survives"
    );
}

/// An owner-scoped save must not drop a record another session owns.
///
/// Shape 0 is owned by `session-a`; shape 1 belongs to `session-b`.
#[test]
fn owner_merge_keeps_another_sessions_record() {
    let persisted = vec![
        merge_owner_record(0, 1, true, "owner-record"),
        merge_owner_record(1, 1, true, "other-session"),
    ];
    let snapshot = vec![merge_owner_record(0, 1, true, "owner-record")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);
    let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
    tags.sort();

    assert_eq!(tags, vec!["other-session".to_owned(), "owner-record".to_owned()]);
}
