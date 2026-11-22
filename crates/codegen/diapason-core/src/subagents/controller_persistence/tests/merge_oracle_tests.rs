//! The ledger merge oracle and canonical-id merge cases.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Provides the record
//! builders and the generated oracle over canonical ids, freshest-record
//! selection and snapshot precedence; the sibling reconciliation and adoption
//! modules reuse its builders.
use super::super::*;
use proptest::prelude::*;

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
pub(super) fn merge_canonical_id(shape: usize) -> String {
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
pub(super) fn merge_owner_record(
    shape: usize,
    updated_offset: u8,
    desired_enabled: bool,
    tag: &str,
) -> PersistedBackgroundRecord {
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
pub(super) fn merge_launch_record(
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
pub(super) fn merge_state(record: &PersistedBackgroundRecord) -> (String, bool, String) {
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

        let merged = merge_unscoped_background_records(persisted_records, snapshot_records, &BTreeMap::new());
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
