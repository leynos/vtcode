//! Precedence between a snapshot launch and the launch the ledger holds.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Pins that a refreshed or
//! stopped copy yields to a launch it does not name, and that a launch this
//! save started supersedes the one it replaced. It reuses the record builders
//! from `merge_oracle_tests.rs`.
use super::super::*;
use super::merge_oracle_tests::{merge_canonical_id, merge_launch_record, merge_state};

/// An equal timestamp keeps the snapshot launch across launches too.
///
/// The tie rule is documented for two copies of one launch, but the same two
/// sides compete when a restart leaves two launches under one canonical id.
/// Without the snapshot's provenance the reduction kept whichever exec session
/// sorted last, so a persisted `exec-z` displaced an equal snapshot `exec-a`.
#[test]
fn an_equal_timestamp_keeps_the_snapshot_launch() {
    // Shape 0 is `worker` owned by `session-a`; both copies name the same
    // `updated_at`, and the persisted side sorts last by exec session id.
    let persisted = vec![merge_launch_record(0, 2, true, "exec-z", "persisted-exec-z")];
    let snapshot = vec![merge_launch_record(0, 2, true, "exec-a", "snapshot-exec-a")];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the two copies describe one canonical record");
    assert_eq!(
        merged[0].exec_session_id, "exec-a",
        "a tie must keep the snapshot launch instead of the later exec-session id"
    );
    assert_eq!(
        merge_state(&merged[0]),
        ("snapshot-exec-a".to_owned(), true, "running".to_owned()),
        "the snapshot copy wins the tie"
    );
}

/// A stale stop must not erase a replacement launch another controller wrote.
///
/// The stop names the launch it stopped, so stop precedence applies only on a
/// matching exec session. A persisted row naming a different launch under the
/// same canonical id is a replacement, and it survives even though the stale
/// stop carries the newer `updated_at`; otherwise a save from a controller
/// that never saw the replacement would delete the replacement launch from the
/// ledger.
#[test]
fn a_stale_stop_does_not_replace_a_different_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![merge_launch_record(0, 1, true, "exec-replacement", "replacement")];
    let snapshot = vec![merge_launch_record(0, 3, false, "exec-stopped", "stale-stop")];
    // The save is stopping `exec-stopped`, the launch the snapshot names.
    let expected_stops = BTreeMap::from([(merge_canonical_id(0), "exec-stopped".to_string())]);

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &expected_stops, &[]);

    assert_eq!(merged.len(), 1);
    assert_eq!(merged[0].exec_session_id, "exec-replacement", "the replacement launch keeps the ledger row");
    assert_eq!(
        merge_state(&merged[0]),
        ("replacement".to_owned(), true, "running".to_owned()),
        "the stale stop must not overwrite a launch it does not name"
    );
}

/// A refreshed earlier launch must not displace a persisted replacement.
///
/// A refresh advances `updated_at` while it keeps the exec session it is
/// refreshing; the persisted ledger row names a later launch under the same
/// canonical id. Treating the newer `updated_at` as proof of supersession
/// overwrote the live replacement with the ended launch, and the next load then
/// reaped the replacement and restored the older process. Only a launch of this
/// save may supersede a different persisted session.
#[test]
fn a_refresh_of_an_earlier_launch_does_not_displace_a_persisted_replacement() {
    // Shape 0 is `worker` owned by `session-a`.
    let mut replacement = merge_launch_record(0, 1, true, "exec-replacement", "replacement");
    replacement.started_at = Some("2026-08-28T00:00:02Z".parse().expect("replacement launch time"));
    let persisted = vec![replacement];
    // The snapshot refreshes `exec-earlier`, a launch that began before the
    // replacement, long after the replacement was written: `updated_at` alone
    // would let the ended launch win.
    let mut earlier = merge_launch_record(0, 9, true, "exec-earlier", "refreshed-earlier");
    earlier.started_at = Some("2026-08-28T00:00:01Z".parse().expect("earlier launch time"));
    let snapshot = vec![earlier];

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &BTreeMap::new(), &[]);

    assert_eq!(merged.len(), 1, "the record id keeps one launch");
    assert_eq!(
        merged[0].exec_session_id, "exec-replacement",
        "a refreshed copy of an earlier launch must not displace the persisted replacement"
    );
    assert_eq!(merge_state(&merged[0]).0, "replacement", "the replacement keeps the ledger row");
}

/// A stop applies to the current persisted launch, not an older colliding row.
///
/// Owner migration can leave an older and a newer enabled launch under one
/// canonical id. The load selects the newer one, so a stop of that launch is a
/// stop of the current row; treating the older row as a replacement launch
/// discarded the stop and made the older launch the enabled record for the next
/// load.
#[test]
fn a_stop_applies_to_the_current_persisted_launch() {
    // Shape 0 is `worker` owned by `session-a`.
    let persisted = vec![
        merge_launch_record(0, 1, true, "exec-1", "older-launch"),
        merge_launch_record(0, 2, true, "exec-2", "current-launch"),
    ];
    // The save stops `exec-2`, the launch a load selects.
    let snapshot = vec![merge_launch_record(0, 3, false, "exec-2", "stopped-current")];
    let expected_stops = BTreeMap::from([(merge_canonical_id(0), "exec-2".to_string())]);

    let merged =
        merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(), &expected_stops, &[]);

    assert_eq!(merged.len(), 1, "the record id keeps one launch");
    assert_eq!(
        merge_state(&merged[0]),
        ("stopped-current".to_owned(), false, "stopped".to_owned()),
        "a stop of the current launch must survive an older colliding row"
    );
    assert_eq!(merged[0].exec_session_id, "exec-2", "the older launch must not become the enabled row");
}
