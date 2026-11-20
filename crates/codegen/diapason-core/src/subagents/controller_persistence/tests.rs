//! Pins the background-ledger merge contract.
//!
//! A save joins another owner's records instead of replacing them, keeps the
//! newest copy of a record both sides hold, and honours cancellation and
//! restart precedence, so concurrent controllers cannot discard each other's
//! state.

use super::*;
use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::background::{background_record_id_for_owner, load_background_state, persist_background_state};
use crate::subagents::{SubagentController, SubagentControllerConfig, TaskOwner};
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;
use proptest::prelude::*;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// The owner/agent shapes whose records must stay distinct after a merge.
///
/// Entries 0 and 1 share an agent name but not an owner, and entry 2 has no
/// owner at all, so a legacy id collides across them.
const MERGE_SHAPES: [(&str, Option<&str>); 4] = [
    ("worker", Some("session-a")),
    ("worker", Some("session-b")),
    ("worker", None),
    ("reviewer", Some("session-a")),
];

/// One generated merge input.
#[derive(Debug, Clone, Copy)]
struct MergeRecord {
    /// Index into [`MERGE_SHAPES`], selecting the agent and owner.
    shape: usize,
    /// Seconds past the fixture epoch at which the record was updated.
    updated_offset: u8,
    /// Whether the record still carries the pre-canonicalization id.
    legacy_id: bool,
}

/// Generates a record with an arbitrary shape, freshness and id form.
fn merge_record() -> impl Strategy<Value = MergeRecord> {
    (0usize..MERGE_SHAPES.len(), 0u8..3u8, any::<bool>()).prop_map(|(shape, updated_offset, legacy_id)| MergeRecord {
        shape,
        updated_offset,
        legacy_id,
    })
}

/// The exec session every generated fixture names.
///
/// Sharing one launch session makes the generated cases target the ownership
/// rule: two shapes with the same agent are copies of one launch unless an
/// owner differs, and one with no owner is the copy a launch controller can
/// still hold after a resumed session migrated the row.
const MERGE_EXEC_SESSION: &str = "exec-session";

/// The canonical key, recomputed from the agent and owner alone.
///
/// The generated names contain only characters that survive sanitization,
/// so this is the documented id format rather than a call back into the
/// production helper the merge uses.
fn merge_canonical_id(shape: usize) -> String {
    let (agent, owner) = MERGE_SHAPES[shape];
    match owner {
        Some(owner) => format!("background-{agent}@{owner}"),
        None => format!("background-{agent}"),
    }
}

/// Renders one persisted record, tagging it so its provenance survives.
fn merge_record_json(record: &MergeRecord, tag: &str) -> serde_json::Value {
    let (agent, owner) = MERGE_SHAPES[record.shape];
    let id = if record.legacy_id {
        format!("background-{agent}")
    } else {
        merge_canonical_id(record.shape)
    };
    serde_json::json!({
        "id": id,
        "owner_session_id": owner,
        "agent_name": agent,
        "display_label": agent,
        "description": "merge fixture",
        "source": "builtin",
        "color": null,
        "session_id": "child-session",
        "exec_session_id": MERGE_EXEC_SESSION,
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": format!("2026-08-28T00:00:0{}Z", record.updated_offset),
        "started_at": null,
        "ended_at": null,
        "pid": null,
        "prompt": "work",
        "summary": tag,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    })
}

/// Deserializes generated records and records each tag's timestamp.
/// The property compares the winning record's timestamp with its input.
fn merge_input(
    records: &[MergeRecord],
    prefix: &str,
    updated_by_tag: &mut BTreeMap<String, i64>,
) -> Vec<PersistedBackgroundRecord> {
    records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let tag = format!("{prefix}{index}");
            let value = merge_record_json(record, &tag);
            updated_by_tag.insert(tag, merge_timestamp_millis(&value["updated_at"]));
            serde_json::from_value(value).expect("generated persisted record")
        })
        .collect()
}

/// Parses a serialized timestamp so comparisons do not depend on format.
fn merge_timestamp_millis(value: &serde_json::Value) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value.as_str().expect("serialized timestamp"))
        .expect("parse serialized timestamp")
        .timestamp_millis()
}

/// Renders one fixture record with an explicit cancellation state.
///
/// A higher `updated_offset` is the fresher copy; a cancelled record mirrors
/// what the stop path writes, with `desired_enabled` cleared.
fn merge_owner_record(shape: usize, updated_offset: u8, desired_enabled: bool, tag: &str) -> PersistedBackgroundRecord {
    let record = MergeRecord { shape, updated_offset, legacy_id: false };
    let mut value = merge_record_json(&record, tag);
    value["desired_enabled"] = serde_json::json!(desired_enabled);
    if !desired_enabled {
        value["status"] = serde_json::json!("stopped");
    }
    serde_json::from_value(value).expect("generated persisted record")
}

/// Renders one fixture record that names a specific launch's exec session.
///
/// The exec session is what tells a cancellation and a restart apart.
fn merge_launch_record(
    shape: usize,
    updated_offset: u8,
    desired_enabled: bool,
    exec_session_id: &str,
    tag: &str,
) -> PersistedBackgroundRecord {
    let record = MergeRecord { shape, updated_offset, legacy_id: false };
    let mut value = merge_record_json(&record, tag);
    value["exec_session_id"] = serde_json::json!(exec_session_id);
    value["desired_enabled"] = serde_json::json!(desired_enabled);
    if !desired_enabled {
        value["status"] = serde_json::json!("stopped");
    }
    serde_json::from_value(value).expect("generated persisted record")
}

/// Reads back a merged record's provenance and cancellation state.
fn merge_state(record: &PersistedBackgroundRecord) -> (String, bool, String) {
    let value = serde_json::to_value(record).expect("serialize merged record");
    (
        value["summary"].as_str().expect("provenance tag").to_owned(),
        value["desired_enabled"].as_bool().expect("desired flag"),
        value["status"].as_str().expect("status").to_owned(),
    )
}

/// Inserts `tag` as the winner for `record`'s canonical key when it is newer.
fn expect_merge_winner(expected: &mut BTreeMap<String, (usize, u8, String)>, record: &MergeRecord, tag: String) {
    let key = merge_canonical_id(record.shape);
    match expected.get_mut(&key) {
        Some(current) if record.updated_offset >= current.1 => *current = (record.shape, record.updated_offset, tag),
        Some(_) => {}
        None => {
            expected.insert(key, (record.shape, record.updated_offset, tag));
        }
    }
}

/// Drops expected keys the ownership rule removes from the merge.
///
/// Every generated fixture names [`MERGE_EXEC_SESSION`], so two shapes with the
/// same agent are copies of one launch. An ownerless shape is dropped when any
/// shape that records an owner names the same agent; a second owner is a
/// different launch and survives.
fn remove_ownerless_shadowed(expected: &mut BTreeMap<String, (usize, u8, String)>) {
    let shadowed = expected
        .iter()
        .filter(|(_, (shape, _, _))| MERGE_SHAPES[*shape].1.is_none())
        .filter(|(_, (shape, _, _))| {
            expected.values().any(|(other, _, _)| {
                MERGE_SHAPES[*other].1.is_some() && MERGE_SHAPES[*other].0 == MERGE_SHAPES[*shape].0
            })
        })
        .map(|(key, _)| key.clone())
        .collect::<Vec<_>>();
    for key in shadowed {
        expected.remove(&key);
    }
}

proptest! {
    /// The unscoped merge is a per-key join against an independent model.
    ///
    /// The model recomputes each canonical id from its agent and owner, keeps
    /// the freshest repeated copy (later on a tie), prefers the snapshot on a
    /// tie, keeps every record only one side holds, and drops the ownerless copy
    /// of a launch another shape records an owner for. It never calls the
    /// production id or freshness helpers, so a contract change disagrees.
    #[test]
    fn unscoped_merge_joins_records_by_canonical_key(
        persisted in prop::collection::vec(merge_record(), 0..=5),
        snapshot in prop::collection::vec(merge_record(), 0..=5),
    ) {
        let mut updated_by_tag = BTreeMap::new();
        let persisted_records = merge_input(&persisted, "p", &mut updated_by_tag);
        let snapshot_records = merge_input(&snapshot, "s", &mut updated_by_tag);

        let mut expected: BTreeMap<String, (usize, u8, String)> = BTreeMap::new();
        for (index, record) in persisted.iter().enumerate() {
            expect_merge_winner(&mut expected, record, format!("p{index}"));
        }
        for (index, record) in snapshot.iter().enumerate() {
            expect_merge_winner(&mut expected, record, format!("s{index}"));
        }
        remove_ownerless_shadowed(&mut expected);

        let merged = merge_unscoped_background_records(persisted_records, snapshot_records);
        let observed = merged
            .iter()
            .map(|record| {
                let value = serde_json::to_value(record).expect("serialize merged record");
                (
                    value["id"].as_str().expect("merged id").to_owned(),
                    value["summary"].as_str().expect("provenance tag").to_owned(),
                    merge_timestamp_millis(&value["updated_at"]),
                )
            })
            .collect::<Vec<_>>();

        prop_assert_eq!(observed.len(), expected.len());
        for (entry, (key, (_, _, tag))) in observed.iter().zip(expected.iter()) {
            prop_assert_eq!(&entry.0, key, "records are canonicalized and ordered by key");
            prop_assert_eq!(&entry.1, tag, "the newest copy wins and a tie keeps the snapshot");
            prop_assert_eq!(Some(&entry.2), updated_by_tag.get(tag), "the winner keeps its own timestamp");
        }
    }
}

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
    let records = [
        merge_launch_record(0, 3, true, "exec-1", "active-exec-1"),
        merge_launch_record(0, 1, false, "exec-1", "cancelled-exec-1"),
        merge_launch_record(0, 2, true, "exec-2", "active-exec-2"),
    ];
    let expected = ("active-exec-2".to_owned(), true, "running".to_owned());

    for order in [[0_usize, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
        let ordered = order.iter().map(|index| records[*index].clone()).collect::<Vec<_>>();
        assert_eq!(
            merged_state(collect_by_canonical_id(ordered.clone()).into_values()),
            expected,
            "the load collapse must select the restarted launch for order {order:?}"
        );
        assert_eq!(
            merged_state(merge_background_records(ordered.clone(), Vec::new(), Some("session-a"), &BTreeSet::new(),)),
            expected,
            "the save merge must select the restarted launch for order {order:?}"
        );
        // The trio also arrives split across the persisted and snapshot sides.
        let mut persisted = ordered;
        let snapshot = persisted.split_off(1);
        assert_eq!(
            merged_state(merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(),)),
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
        assert_eq!(
            merged_state(merge_background_records(ordered.clone(), Vec::new(), Some("session-a"), &BTreeSet::new(),)),
            expected,
            "the save merge must keep the newer restart for order {order:?}"
        );
        let mut persisted = ordered;
        let snapshot = persisted.split_off(1);
        assert_eq!(
            merged_state(merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new(),)),
            expected,
            "the restart must win whichever side carries it"
        );
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
        let merged = merge_background_records(persisted, snapshot, None, &BTreeSet::new());
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

    let merged = merge_background_records(vec![ownerless_cancelled], vec![owned], None, &BTreeSet::new());

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

    let merged = merge_background_records(vec![owned], vec![ownerless], None, &BTreeSet::new());
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

    let merged = merge_background_records(persisted, Vec::new(), None, &BTreeSet::new());
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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());

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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());

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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());

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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());

    assert_eq!(merged.len(), 1);
    assert_eq!(
        merge_state(&merged[0]),
        ("restarted".to_owned(), true, "running".to_owned()),
        "a restart names a new exec session and is not the stale copy a stop outranks"
    );
}

/// Colliding persisted ids are resolved by `updated_at`, not file order.
///
/// A ledger rewritten by an older build can hold a legacy id for one record
/// after both canonicalize; the stale copy is written last here and must lose.
#[test]
fn colliding_persisted_ids_keep_the_newest_record() {
    // Shape 0 is `worker` owned by `session-a`.
    let fresh = MergeRecord { shape: 0, updated_offset: 2, legacy_id: false };
    let stale = MergeRecord { shape: 0, updated_offset: 1, legacy_id: true };
    let persisted = vec![
        serde_json::from_value(merge_record_json(&fresh, "fresh")).expect("fresh record"),
        serde_json::from_value(merge_record_json(&stale, "stale")).expect("stale record"),
    ];

    let merged = collect_by_canonical_id(persisted);

    assert_eq!(merged.len(), 1, "both ids name one record after canonicalization");
    let record = merged.values().next().expect("the merged record");
    assert_eq!(merge_state(record).0, "fresh", "file order must not discard the newer copy");
}

/// Collapsing duplicate id forms must not discard a cancellation.
///
/// A newer active copy can sit under the canonical id while the stop still
/// carries the legacy id. Whichever form holds it, the stop must survive so
/// the merge can apply its precedence.
#[test]
fn colliding_persisted_ids_keep_a_cancellation_over_a_newer_active_copy() {
    // Shape 0 is `worker` owned by `session-a`; both copies name `exec-session`.
    let mut cancelled = merge_owner_record(0, 1, false, "cancelled");
    cancelled.id = "background-worker".to_string();
    let active = merge_owner_record(0, 2, true, "stale-active");

    for records in [
        vec![active.clone(), cancelled.clone()],
        vec![cancelled.clone(), active.clone()],
    ] {
        let merged = collect_by_canonical_id(records);
        assert_eq!(merged.len(), 1, "both ids name one record after canonicalization");
        let record = merged.values().next().expect("the merged record");
        assert_eq!(
            merge_state(record),
            ("cancelled".to_owned(), false, "stopped".to_owned()),
            "a newer active copy must not discard the stop while collapsing duplicate ids"
        );
    }
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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &removable);

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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());
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

    let merged = merge_background_records(persisted, Vec::new(), Some("session-a"), &removable);

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

    let merged = merge_background_records(persisted, snapshot, Some("session-a"), &BTreeSet::new());
    let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
    tags.sort();

    assert_eq!(tags, vec!["other-session".to_owned(), "owner-record".to_owned()]);
}

/// Builds a controller for a temporary workspace.
///
/// The fixture owns no provider access: these tests exercise ledger loads,
/// saves and cancellations, which never reach a model.
fn ownership_controller_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), DiapasonConfig::default().pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: "gpt-5.6-sol".to_string(),
        parent_provider: "openai".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: "test-key".to_string(),
        vt_cfg: DiapasonConfig::default(),
        openai_chatgpt_auth: None,
        depth: 0,
        workspace_gated: false,
        exec_sessions,
        output_reader: None,
        pty_manager: pty_sessions.manager().clone(),
        managed_background_runtime: false,
        lock_scope: None,
    }
}

/// Renders the ownerless row an older build wrote for a background launch.
fn legacy_row(parent_session_id: &str) -> PersistedBackgroundRecord {
    serde_json::from_value(serde_json::json!({
        "id": "background-worker",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "owner_session_id": null,
        "session_id": "child-session",
        "parent_session_id": parent_session_id,
        "exec_session_id": "exec-launch",
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": "2026-08-28T00:00:00Z",
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
    .expect("legacy persisted record")
}

/// Writes a ledger the resume migration has already given to `owner`.
async fn migrate_ledger_to(workspace: &Path, owner: &str) {
    let mut state = load_background_state(workspace).await.expect("read seeded ledger");
    let record = state.records.first_mut().expect("the seeded record");
    record.owner_session_id = Some(owner.to_string());
    persist_background_state(workspace, state.records)
        .await
        .expect("write migrated ledger");
}

/// A launch controller adopts a migrated launch, saves it once, and cannot
/// resurrect it after the owning session stops it.
///
/// This is the ordering the ACP launch runtime creates: the workspace
/// controller loads the ownerless row before the session migrates it, so its
/// in-memory copy is stale. Saving that copy without reconciliation would write
/// an ownerless record beside the migrated one, which a later restore could
/// relaunch after the session cancelled the task.
#[tokio::test]
async fn a_migrated_launch_is_adopted_and_never_revived_by_a_workspace_save() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    let before = workspace_controller.background_status_entries().await;
    assert_eq!(before.len(), 1, "the workspace controller loads the ownerless row");
    assert_eq!(before[0].owner_session_id, None, "the loaded copy is still ownerless");
    assert_eq!(before[0].exec_session_id, "exec-launch", "the launch must not be replaced");

    migrate_ledger_to(temp.path(), owner).await;

    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adopt the migrated launch"),
        1,
        "the stale ownerless copy joins the migrated record"
    );
    workspace_controller
        .save_background_ledger()
        .await
        .expect("save the adopted launch");

    let owned_id = background_record_id_for_owner("worker", Some(owner));
    let entries = workspace_controller.background_status_entries().await;
    assert_eq!(entries.len(), 1, "adoption rekeys the record rather than duplicating it");
    assert_eq!(entries[0].id, owned_id, "the record is keyed by its owned canonical id");
    assert_eq!(entries[0].owner_session_id.as_deref(), Some(owner));
    let ledger = load_background_state(temp.path()).await.expect("read the saved ledger");
    assert_eq!(ledger.records.len(), 1, "the workspace save must not write an ownerless duplicate");
    assert_eq!(ledger.records[0].id, owned_id);
    assert_eq!(ledger.records[0].owner_session_id.as_deref(), Some(owner));

    let session_controller = SubagentController::new_with_background_owner(
        ownership_controller_config(temp.path().to_path_buf()),
        Some(owner),
    )
    .await
    .expect("session controller");
    session_controller
        .force_cancel_background(TaskOwner::Session(owner), &owned_id)
        .await
        .expect("cancel the owned launch");

    workspace_controller
        .save_background_ledger()
        .await
        .expect("save after the session stopped the launch");

    let ledger = load_background_state(temp.path())
        .await
        .expect("read the ledger after cancellation");
    assert_eq!(ledger.records.len(), 1, "a cancelled launch must leave nothing restorable");
    assert_eq!(ledger.records[0].owner_session_id.as_deref(), Some(owner));
    let persisted = serde_json::to_value(&ledger.records[0]).expect("render the persisted record");
    assert_eq!(
        persisted["desired_enabled"],
        serde_json::json!(false),
        "a workspace save must not revive the launch the session stopped"
    );
}

/// A failed migration adopts nothing.
///
/// The ledger still names the row ownerless, so the launch controller must not
/// invent the owner the migration failed to write; a later retry is the only
/// step that may attribute it.
#[tokio::test]
async fn adoption_waits_for_a_ledger_that_records_the_owner() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");

    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "an ownerless ledger must not be attributed by the controller"
    );
    let entries = workspace_controller.background_status_entries().await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].owner_session_id, None, "the copy stays ownerless as the ledger says");
}

/// An owner-scoped controller adopts nothing.
///
/// It already loads only its own rows, so a workspace-wide adoption pass would
/// give it authority over records it never had.
#[tokio::test]
async fn only_an_unscoped_controller_adopts_rows() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");
    migrate_ledger_to(temp.path(), owner).await;

    let session_controller = SubagentController::new_with_background_owner(
        ownership_controller_config(temp.path().to_path_buf()),
        Some(owner),
    )
    .await
    .expect("session controller");

    assert_eq!(
        session_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "an owner-scoped controller has no ownerless copies to adopt"
    );
}
