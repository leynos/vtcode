//! Ledger rows with an empty exec session and the identity that replaces it.
//!
//! Declared by `controller_persistence/tests/mod.rs`. A legacy row names no
//! exec session, so the child session it carries is the launch identity left.
//! Two rows naming the same child are copies of one launch and resolve by the
//! ordinary stop rule; a row that names neither is ambiguous only when another
//! unattributed row shares its canonical id. It reuses the record builders
//! from `merge_oracle_tests.rs`.
use std::collections::{BTreeMap, BTreeSet};

use super::super::merge::merge_background_records;
use super::super::*;
use super::merge_oracle_tests::{merge_canonical_id, merge_launch_record, merge_state};

/// Two rows with no launch identity under one canonical id are not one launch.
///
/// The canonical id is derived from the agent and owner, so two persisted rows
/// that name neither an exec session nor a child session occupy one launch
/// slot. In that slot a cancelled row outranks a desired one whatever their
/// timestamps, so the desired task was silently not restored. Nothing can tell
/// the rows apart and the map key holds one record per canonical id, so the
/// ambiguous id is dropped rather than resolving it arbitrarily.
#[test]
fn two_rows_without_a_launch_identity_are_not_one_launch() {
    // Shape 0 is `worker` owned by `session-a`; both rows share its canonical id
    // and differ only in intent and freshness.
    let mut stopped = merge_launch_record(0, 2, false, "", "stopped");
    stopped.session_id = String::new();
    let mut desired = merge_launch_record(0, 1, true, "", "desired");
    desired.session_id = String::new();

    for records in [
        vec![stopped.clone(), desired.clone()],
        vec![desired.clone(), stopped.clone()],
    ] {
        let merged = collect_by_canonical_id(records);
        assert!(
            merged.is_empty(),
            "an id two rows share without a launch identity must not be resolved arbitrarily: {merged:?}"
        );
    }
}

/// Two legacy copies that name one child session resolve to their stop.
///
/// An empty exec session alone does not make the id ambiguous. Both rows name
/// the same child session, which every launch mints afresh, so they are copies
/// of one launch and the older enabled copy yields to the stop. Dropping the id
/// would instead lose the stop from both the load and the save.
#[test]
fn two_legacy_copies_of_one_child_session_resolve_to_their_stop() {
    // Shape 0 is `worker` owned by `session-a`; the fixtures share one child
    // session and differ only in intent and freshness.
    let stopped = merge_launch_record(0, 2, false, "", "stopped");
    let refreshed = merge_launch_record(0, 1, true, "", "refreshed");
    assert_eq!(stopped.session_id, refreshed.session_id, "the fixtures name one child session");

    let loaded = collect_by_canonical_id(vec![refreshed.clone(), stopped.clone()]);
    assert_eq!(loaded.len(), 1, "the shared child session is one launch");
    let record = loaded.values().next().expect("the surviving copy");
    assert_eq!(
        merge_state(record),
        ("stopped".to_owned(), false, "stopped".to_owned()),
        "the load must keep the stop of the launch both rows name"
    );

    let merged = merge_background_records(
        vec![refreshed, stopped],
        Vec::new(),
        Some("session-a"),
        &BTreeSet::new(),
        &BTreeMap::new(),
        &[],
    );
    assert_eq!(merged.len(), 1, "the save keeps one copy of the launch");
    assert_eq!(
        merge_state(&merged[0]),
        ("stopped".to_owned(), false, "stopped".to_owned()),
        "the save must keep the stop of the launch both rows name"
    );
}

/// A single row with no launch identity still loads.
///
/// Only a duplicate is ambiguous; one row is the record it says it is, and an
/// older build may only ever have written one. The row names neither an exec
/// session nor a child session, so this is the boundary the rule is about: a
/// loader that discarded every unattributed row would return nothing here.
#[test]
fn a_single_empty_launch_row_still_loads() {
    // Shape 0 is `worker` owned by `session-a`; both identities are cleared so
    // the fixture is the unattributed row the test describes.
    let mut record = merge_launch_record(0, 1, true, "", "only-row");
    record.session_id = String::new();

    let merged = collect_by_canonical_id(vec![record]);

    assert_eq!(merged.len(), 1, "one row is unambiguous");
    assert_eq!(merge_state(merged.values().next().expect("the record")).0, "only-row");
}

/// A live snapshot record survives ambiguous persisted rows under its id.
///
/// The ledger holds two rows that share a canonical id and carry no launch
/// identity, so they cannot be told apart and stay excluded. A snapshot copy
/// that names its own exec session is a launch this controller is running; it
/// must not be removed with them, or the merged ledger omits an active task.
#[test]
fn a_live_snapshot_record_survives_ambiguous_persisted_rows() {
    // Shape 0 is `worker` owned by `session-a`; the persisted rows share its
    // canonical id and name no launch identity at all.
    let mut stopped = merge_launch_record(0, 2, false, "", "stopped");
    stopped.session_id = String::new();
    let mut desired = merge_launch_record(0, 1, true, "", "desired");
    desired.session_id = String::new();
    let live = merge_launch_record(0, 3, true, "exec-live", "live");

    let merged =
        merge_background_records(vec![stopped, desired], vec![live], None, &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the snapshot launch keeps the canonical id");
    let record = merged.first().expect("the surviving launch");
    assert_eq!(
        record.exec_session_id, "exec-live",
        "a live snapshot record must not be dropped with ambiguous persisted rows"
    );
    assert_eq!(
        merge_state(record),
        ("live".to_owned(), true, "running".to_owned()),
        "the surviving copy is the snapshot's launch"
    );
}

/// A legacy stop does not govern a snapshot naming another child session.
///
/// Both rows carry an empty exec session, but they name different child
/// sessions, so they are different launches. Grouping them as one launch let
/// the stop cancel a launch it never described; the newer launch must instead
/// supersede the older one under the ordinary restart rule.
#[test]
fn a_legacy_stop_does_not_govern_a_snapshot_naming_another_child_session() {
    // Shape 0 is `worker` owned by `session-a`; the fixtures share one child
    // session unless a case names another.
    let stopped = merge_launch_record(0, 1, false, "", "persisted-stop");
    let mut restarted = merge_launch_record(0, 3, true, "", "snapshot-restart");
    restarted.session_id = "child-session-restarted".to_string();

    let merged =
        merge_background_records(vec![stopped], vec![restarted], None, &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the ledger keeps one row per canonical id");
    assert_eq!(
        merge_state(&merged[0]),
        ("snapshot-restart".to_owned(), true, "running".to_owned()),
        "the newer launch survives; another launch's stop does not cancel it"
    );
}

/// A legacy stop still governs a refresh of the child session it stopped.
///
/// The intended handling of a genuine legacy stop: both rows name the same
/// child session, so they are copies of one launch and the stop outranks the
/// refresh whatever its timestamp.
#[test]
fn a_legacy_stop_still_governs_a_refresh_of_its_own_child_session() {
    let stopped = merge_launch_record(0, 1, false, "", "persisted-stop");
    let refreshed = merge_launch_record(0, 3, true, "", "snapshot-refresh");

    let merged =
        merge_background_records(vec![stopped], vec![refreshed], None, &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]),
        ("persisted-stop".to_owned(), false, "stopped".to_owned()),
        "a refresh must not resurrect the legacy launch it stopped"
    );
}

/// A legacy stop must not replace another child's persisted copy.
///
/// Both rows name no exec session, so the cross-launch checks used to compare
/// empty with empty and treat the stop as a copy of whatever launch the ledger
/// held under the canonical id. The child session each names is the launch
/// identity: a stop governs only its own child, so the different child's
/// persisted row survives even though the stop is newer and records a start.
#[test]
fn a_legacy_stop_does_not_replace_a_persisted_copy_of_another_child_session() {
    // Shape 0 is `worker` owned by `session-a`. The ledger holds child B while
    // the save is stopping child A.
    let mut persisted = merge_launch_record(0, 1, true, "", "persisted-child-b");
    persisted.session_id = "child-b".to_string();
    let mut stop = merge_launch_record(0, 3, false, "", "stop-child-a");
    stop.session_id = "child-a".to_string();
    stop.started_at = Some("2026-08-28T00:00:03Z".parse().expect("stop launch time"));
    // The save records that it is stopping a launch with no exec session.
    let expected_stops = BTreeMap::from([(merge_canonical_id(0), String::new())]);

    let merged = merge_background_records(
        vec![persisted],
        vec![stop],
        Some("session-a"),
        &BTreeSet::new(),
        &expected_stops,
        &[],
    );

    assert_eq!(merged.len(), 1, "the canonical id keeps one launch");
    assert_eq!(
        merged[0].session_id, "child-b",
        "a stop of another child session must not replace the persisted launch"
    );
    assert!(merged[0].desired_enabled, "the other child's persisted launch must stay wanted");
    assert_eq!(merge_state(&merged[0]).0, "persisted-child-b");
}
