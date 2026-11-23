//! Ambiguous ledger rows that carry no launch identity.
//!
//! Declared by `controller_persistence/tests/mod.rs`. A canonical id names one
//! record, so two rows under one id with an empty exec session cannot be told
//! apart; the load must not resolve them by cancellation precedence. It reuses
//! the record builders from `merge_oracle_tests.rs`.
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
