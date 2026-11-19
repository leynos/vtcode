use super::*;
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
        "exec_session_id": "exec-session",
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
fn expect_merge_winner(expected: &mut BTreeMap<String, (u8, String)>, record: &MergeRecord, tag: String) {
    let key = merge_canonical_id(record.shape);
    match expected.get_mut(&key) {
        Some(current) if record.updated_offset >= current.0 => *current = (record.updated_offset, tag),
        Some(_) => {}
        None => {
            expected.insert(key, (record.updated_offset, tag));
        }
    }
}

proptest! {
    /// The unscoped merge is a per-key join against an independent model.
    ///
    /// The model recomputes each canonical id from its agent and owner, keeps
    /// the freshest repeated copy (later on a tie), prefers the snapshot on a
    /// tie, and keeps every record only one side holds. It never calls the
    /// production id or freshness helpers, so a contract change disagrees.
    #[test]
    fn unscoped_merge_joins_records_by_canonical_key(
        persisted in prop::collection::vec(merge_record(), 0..=5),
        snapshot in prop::collection::vec(merge_record(), 0..=5),
    ) {
        let mut updated_by_tag = BTreeMap::new();
        let persisted_records = merge_input(&persisted, "p", &mut updated_by_tag);
        let snapshot_records = merge_input(&snapshot, "s", &mut updated_by_tag);

        let mut expected: BTreeMap<String, (u8, String)> = BTreeMap::new();
        for (index, record) in persisted.iter().enumerate() {
            expect_merge_winner(&mut expected, record, format!("p{index}"));
        }
        for (index, record) in snapshot.iter().enumerate() {
            expect_merge_winner(&mut expected, record, format!("s{index}"));
        }

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
        for (entry, (key, (_, tag))) in observed.iter().zip(expected.iter()) {
            prop_assert_eq!(&entry.0, key, "records are canonicalized and ordered by key");
            prop_assert_eq!(&entry.1, tag, "the newest copy wins and a tie keeps the snapshot");
            prop_assert_eq!(Some(&entry.2), updated_by_tag.get(tag), "the winner keeps its own timestamp");
        }
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
