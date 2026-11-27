//! Ownership precedence and removal authority in the ledger merge.
//!
//! Pins that a superseded owned launch leaves no ownerless copy behind, that an
//! owned copy outranks an ownerless copy of the same launch, and that an
//! owner-scoped save removes only the ids it has authority over. Declared by
//! `controller_persistence/tests/mod.rs`; it reuses the builders in
//! `merge_oracle_tests.rs`.
use super::super::launch::join_by_canonical_id;
use super::super::*;
use super::merge_oracle_tests::{merge_canonical_id, merge_launch_record, merge_owner_record, merge_state};
use crate::subagents::background::background_record_id_for_owner;
use serde_json::json;

/// Builds one persisted record with the canonical id for its agent/owner.
fn persisted(
    agent: &str,
    owner_session_id: Option<&str>,
    exec_session_id: &str,
    updated_at: &str,
) -> PersistedBackgroundRecord {
    serde_json::from_value(json!({
        "id": background_record_id_for_owner(agent, owner_session_id),
        "agent_name": agent,
        "display_label": agent,
        "description": agent,
        "source": "builtin",
        "color": null,
        "owner_session_id": owner_session_id,
        "session_id": format!("session-{exec_session_id}"),
        "parent_session_id": owner_session_id.unwrap_or("parent-session"),
        "exec_session_id": exec_session_id,
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": updated_at,
        "started_at": null,
        "ended_at": null,
        "pid": null,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    }))
    .expect("persisted background record")
}

/// A newer owned launch supersedes an older owned launch of the same agent.
///
/// The older launch's ownerless copy has no owned survivor left to match,
/// so collecting ownership evidence only from the reduced launches kept it
/// beside the replacement. The unrelated ownerless record is not evidence
/// of a migrated launch and must survive.
#[test]
fn an_ownerless_copy_of_a_superseded_owned_launch_is_dropped() {
    let ledger = vec![
        persisted("worker", None, "exec-1", "2026-08-28T00:00:00Z"),
        persisted("worker", Some("session-a"), "exec-1", "2026-08-28T00:00:00Z"),
        persisted("outside", None, "exec-outside", "2026-08-28T00:00:00Z"),
    ];
    let snapshot = vec![persisted("worker", Some("session-a"), "exec-2", "2026-08-29T00:00:00Z")];

    let merged = join_by_canonical_id(ledger, snapshot, &BTreeMap::new(), &[]);

    assert!(
        merged
            .values()
            .all(|record| !(record.owner_session_id.is_none() && record.exec_session_id == "exec-1")),
        "the ownerless copy of the superseded launch must not survive: {merged:?}"
    );
    assert!(
        merged
            .values()
            .any(|record| record.owner_session_id.is_none() && record.exec_session_id == "exec-outside"),
        "an ownerless record no owned copy names must survive: {merged:?}"
    );
    let owned = merged
        .values()
        .filter(|record| record.owner_session_id.as_deref() == Some("session-a"))
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), 1, "the newer owned launch is the only owned survivor");
    assert_eq!(owned[0].exec_session_id, "exec-2");
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
