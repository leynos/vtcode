//! Ambiguous ledger rows that carry no launch identity.
//!
//! Declared by `controller_persistence/tests/mod.rs`. A canonical id names one
//! record, so two rows under one id with an empty exec session cannot be told
//! apart; the load must not resolve them by cancellation precedence. It reuses
//! the record builders from `merge_oracle_tests.rs`.
use std::collections::{BTreeMap, BTreeSet};

use super::super::merge::merge_background_records;
use super::super::*;
use super::merge_oracle_tests::{merge_launch_record, merge_state};

/// Two rows sharing a canonical id with no launch identity are not one launch.
///
/// The canonical id is derived from the agent and owner, so two persisted rows
/// that both carry an empty `exec_session_id` occupy one launch slot. In that
/// slot a cancelled row outranks a desired one whatever their timestamps, so
/// the desired task was silently not restored. An empty exec session cannot
/// identify a launch, and the map key holds one record per canonical id, so the
/// ambiguous id is dropped rather than resolving it arbitrarily.
#[test]
fn two_empty_launch_rows_under_one_canonical_id_are_not_one_launch() {
    // Shape 0 is `worker` owned by `session-a`; both rows share its canonical id
    // and differ only in intent and freshness.
    let stopped = merge_launch_record(0, 2, false, "", "stopped");
    let desired = merge_launch_record(0, 1, true, "", "desired");

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

/// A single row with no launch identity still loads.
///
/// Only a duplicate is ambiguous; one row is the record it says it is, and an
/// older build may only ever have written one.
#[test]
fn a_single_empty_launch_row_still_loads() {
    // Shape 0 is `worker` owned by `session-a`.
    let record = merge_launch_record(0, 1, true, "", "only-row");

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
    // canonical id and differ only in intent and freshness.
    let stopped = merge_launch_record(0, 2, false, "", "stopped");
    let desired = merge_launch_record(0, 1, true, "", "desired");
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
