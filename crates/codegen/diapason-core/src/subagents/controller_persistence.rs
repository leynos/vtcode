//! Persisting one controller's background ledger without losing another owner's records.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;

use super::background::{load_background_state, persist_background_state};
use super::types::PersistedBackgroundRecord;

/// Reloads the shared ledger, merges one controller's snapshot and writes it.
///
/// The caller holds both persistence locks; this function owns the reload,
/// merge and write so they complete as one critical section.
///
/// The ledger is shared with the owner-scoped controllers of other sessions
/// and processes, so a save is a merge rather than a replacement. A record
/// another owner created after this controller loaded survives, and a record
/// both sides hold keeps the copy with the newest `updated_at`. An owner-scoped
/// save additionally removes the records its session held in the ledger but no
/// longer carries, so an owner-side removal still reaches disk.
pub(super) async fn persist_background_records(
    workspace_root: &Path,
    owner_session_id: Option<&str>,
    records: Vec<PersistedBackgroundRecord>,
) -> Result<()> {
    let persisted_records = load_background_state(workspace_root).await?.records;
    let merged = match owner_session_id {
        // An owner-scoped controller supplies the authoritative set of records
        // its own session still tracks, so a record it dropped is removed from
        // the ledger. It must not replace the copies it holds: an unscoped
        // controller shares the ledger and can cancel one of this owner's
        // tasks, persisting a cancellation newer than the stale copy this
        // controller loaded.
        Some(owner_session_id) => merge_background_records(persisted_records, records, Some(owner_session_id)),
        // An unscoped controller loads other owners' records so it can list and
        // route them, and it owns none of them. Replacing the file with its own
        // snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from its stale copy.
        None => merge_unscoped_background_records(persisted_records, records),
    };
    persist_background_state(workspace_root, merged).await
}

/// Collects persisted records by canonical id, keeping the freshest copy.
///
/// Several entries can canonicalize to one key when a legacy ledger holds both
/// a pre-canonicalization id and its current form. The newest `updated_at`
/// wins and a tie keeps the later entry, so neither the ledger's file order nor
/// the order a controller happens to rebuild its map in decides which copy of a
/// record survives. The load path and the save merge share this rule.
pub(super) fn collect_by_canonical_id(
    records: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    let mut collected: BTreeMap<String, PersistedBackgroundRecord> = BTreeMap::new();
    for mut record in records {
        record.canonicalize_id();
        match collected.get_mut(&record.id) {
            Some(current) if record.is_at_least_as_new_as(current) => *current = record,
            Some(_) => {}
            None => {
                collected.insert(record.id.clone(), record);
            }
        }
    }
    collected
}

/// Merges an unscoped controller's snapshot into the persisted ledger.
///
/// The unscoped controller is not the owner of the owner-scoped records it
/// loads, so it must not replace them with its own copy. This is the unified
/// merge with no removed owner: records it does not track are kept exactly as
/// another process last wrote them, and a record both sides hold keeps the copy
/// with the newest `updated_at`.
fn merge_unscoped_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    merge_background_records(persisted, snapshot, None)
}

/// Merges one controller's snapshot into the persisted ledger.
///
/// The snapshot side is canonicalized and joined to the persisted side by
/// canonical id, keeping the copy with the newest `updated_at`; a tie keeps the
/// snapshot, because an untouched snapshot equals the copy this controller
/// loaded while a local refresh or cancellation is newer than it. A record only
/// one side holds is kept, so no controller discards a record another owner
/// created after it loaded.
///
/// A persisted stop outranks the snapshot when the two name the same exec
/// session, even if the snapshot was written later: a refresh advances
/// `updated_at` without changing intent, so timestamp order alone would let a
/// stale copy resurrect a cancelled task. A restart replaces the exec session,
/// so it is never mistaken for that stale copy.
///
/// `removed_owner` names the session whose snapshot is authoritative for its
/// own records. When it is `Some`, only that session's persisted records are
/// canonicalized and joined: a ledger written by an older build can hold a
/// legacy id for the same record, and the load path canonicalizes in memory, so
/// keying on the raw id would let one record appear twice. Every other
/// persisted record is carried verbatim, because another session's record, and
/// an ownerless legacy record, belong to a writer that is not this controller;
/// rewriting their ids would rename records this controller never owned. The
/// owner's records are then removed unless the snapshot still carries their
/// canonical id, which propagates an owner-side removal to disk.
fn merge_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
    removed_owner: Option<&str>,
) -> Vec<PersistedBackgroundRecord> {
    let mut carried_verbatim: Vec<PersistedBackgroundRecord> = Vec::new();
    let mut owned: Vec<PersistedBackgroundRecord> = Vec::new();
    for record in persisted {
        match removed_owner {
            Some(owner) if record.owner_session_id.as_deref() != Some(owner) => carried_verbatim.push(record),
            _ => owned.push(record),
        }
    }
    let mut merged = collect_by_canonical_id(owned);
    let mut snapshot_ids: BTreeSet<String> = BTreeSet::new();
    for mut record in snapshot {
        record.canonicalize_id();
        let id = record.id.clone();
        match merged.get_mut(&id) {
            Some(current) if current.cancellation_outranks(&record) => {}
            Some(current) if record.is_at_least_as_new_as(current) => *current = record,
            Some(_) => {}
            None => {
                merged.insert(id.clone(), record);
            }
        }
        snapshot_ids.insert(id);
    }
    if let Some(owner) = removed_owner {
        merged.retain(|id, record| record.owner_session_id.as_deref() != Some(owner) || snapshot_ids.contains(id));
    }
    carried_verbatim.extend(merged.into_values());
    carried_verbatim
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The owner/agent shapes whose records must stay distinct after a merge.
    ///
    /// Entries 0 and 1 share an agent name but not an owner, and entry 2 is the
    /// same agent with no owner at all. A legacy (unscoped) id therefore
    /// collides across them, which is exactly what canonicalization must
    /// separate rather than collapse.
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
        (0usize..MERGE_SHAPES.len(), 0u8..3u8, any::<bool>())
            .prop_map(|(shape, updated_offset, legacy_id)| MergeRecord { shape, updated_offset, legacy_id })
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

    /// Deserializes generated records and records each tag's own timestamp.
    ///
    /// The ledger keeps the tags so the property can compare the winning
    /// record's timestamp with the input it came from.
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
    /// `updated_offset` drives the fixture's `updated_at`, so a higher value is
    /// the fresher copy. A cancelled record mirrors what the stop path writes:
    /// `desired_enabled` is cleared and the status is no longer active.
    fn merge_owner_record(
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
    /// The launch is what a cancellation and a restart are told apart by, so a
    /// test that orders them needs to say which session each copy describes.
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

    proptest! {
        /// The unscoped merge is a per-key join against an independent model.
        ///
        /// The model canonicalizes each generated id from its agent and owner,
        /// keeps the freshest copy a ledger repeats (the later entry on a tie),
        /// prefers the snapshot on a tie with the persisted side, and keeps
        /// every record only one side holds. It never calls the production id or
        /// freshness helpers, so a change to either contract shows up as a
        /// disagreement rather than being mirrored by the test.
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
                let key = merge_canonical_id(record.shape);
                let tag = format!("p{index}");
                match expected.get_mut(&key) {
                    Some(current) if record.updated_offset >= current.0 => *current = (record.updated_offset, tag),
                    Some(_) => {}
                    None => {
                        expected.insert(key, (record.updated_offset, tag));
                    }
                }
            }
            for (index, record) in snapshot.iter().enumerate() {
                let key = merge_canonical_id(record.shape);
                let tag = format!("s{index}");
                match expected.get_mut(&key) {
                    Some(current) if record.updated_offset >= current.0 => *current = (record.updated_offset, tag),
                    Some(_) => {}
                    None => {
                        expected.insert(key, (record.updated_offset, tag));
                    }
                }
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
    /// An unscoped controller shares this owner's ledger, so it can cancel a
    /// background task and persist that cancellation first. The owner then saves
    /// the stale active copy it still holds. The newer persisted cancellation has
    /// to win, because restore selection reads `desired_enabled`.
    #[test]
    fn owner_merge_keeps_a_newer_cancellation() {
        // Shape 0 is `worker` owned by `session-a`.
        let persisted = vec![merge_owner_record(0, 2, false, "cancelled")];
        let snapshot = vec![merge_owner_record(0, 1, true, "stale-active")];

        let merged = merge_background_records(persisted, snapshot, Some("session-a"));

        assert_eq!(merged.len(), 1);
        assert_eq!(merge_state(&merged[0]), ("cancelled".to_owned(), false, "stopped".to_owned()));
    }

    /// A stop outranks a later refresh of the same launch.
    ///
    /// A refresh advances `updated_at` without changing intent, so the owner's
    /// copy can look newer than the cancellation another controller wrote for
    /// the same exec session. Timestamp order alone would then resurrect the
    /// stopped task, so the persisted stop wins even though it is older.
    #[test]
    fn owner_merge_keeps_a_cancellation_a_refresh_outdates() {
        // Shape 0 is `worker` owned by `session-a`.
        let persisted = vec![merge_launch_record(0, 1, false, "exec-launch-1", "cancelled")];
        let snapshot = vec![merge_launch_record(0, 2, true, "exec-launch-1", "refreshed")];

        let merged = merge_background_records(persisted, snapshot, Some("session-a"));

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merge_state(&merged[0]),
            ("cancelled".to_owned(), false, "stopped".to_owned()),
            "a refresh of the stopped launch must not undo the cancellation"
        );
    }

    /// A restart replaces the launch, so its snapshot is not a stale copy.
    ///
    /// The persisted cancellation names the exec session that was stopped. A
    /// snapshot that names a different session is a replacement launch the
    /// owner has since written, and it must be kept so an explicit restart
    /// survives its own save.
    #[test]
    fn owner_merge_accepts_a_restart_with_a_new_exec_session() {
        // Shape 0 is `worker` owned by `session-a`.
        let persisted = vec![merge_launch_record(0, 2, false, "exec-launch-1", "cancelled")];
        let snapshot = vec![merge_launch_record(0, 3, true, "exec-launch-2", "restarted")];

        let merged = merge_background_records(persisted, snapshot, Some("session-a"));

        assert_eq!(merged.len(), 1);
        assert_eq!(
            merge_state(&merged[0]),
            ("restarted".to_owned(), true, "running".to_owned()),
            "a restart names a new exec session and is not the stale copy a stop outranks"
        );
    }

    /// Colliding persisted ids are resolved by `updated_at`, not file order.
    ///
    /// A legacy ledger can hold `background-worker` and
    /// `background-worker@session-a` for one record once both canonicalize. The
    /// stale legacy copy is written last here, as a ledger rewritten by an
    /// older build leaves it, and must still lose to the newer canonical copy.
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

    /// A record the owner no longer tracks is still removed from the ledger.
    ///
    /// The snapshot is authoritative for which of the owner's records remain, so
    /// a record the owner dropped must not survive merely because the persisted
    /// ledger still holds it. A record both sides hold keeps the snapshot on a
    /// tie, exactly as the unscoped merge does.
    #[test]
    fn owner_merge_removes_a_record_the_snapshot_dropped() {
        // Shape 0 is `worker` and shape 3 is `reviewer`, both owned by `session-a`.
        let persisted = vec![
            merge_owner_record(0, 1, true, "dropped"),
            merge_owner_record(3, 1, true, "kept"),
        ];
        let snapshot = vec![merge_owner_record(3, 1, true, "kept-snapshot")];

        let merged = merge_background_records(persisted, snapshot, Some("session-a"));

        assert_eq!(merged.len(), 1);
        assert_eq!(merge_state(&merged[0]).0, "kept-snapshot");
    }

    /// An owner-scoped save must not drop a record another session owns.
    ///
    /// Shape 0 is `worker` owned by `session-a`; shape 1 is `worker` owned by
    /// `session-b`, whose record this owner's snapshot never carries.
    #[test]
    fn owner_merge_keeps_another_sessions_record() {
        let persisted = vec![
            merge_owner_record(0, 1, true, "owner-record"),
            merge_owner_record(1, 1, true, "other-session"),
        ];
        let snapshot = vec![merge_owner_record(0, 1, true, "owner-record")];

        let merged = merge_background_records(persisted, snapshot, Some("session-a"));
        let mut tags = merged.iter().map(|record| merge_state(record).0).collect::<Vec<_>>();
        tags.sort();

        assert_eq!(tags, vec!["other-session".to_owned(), "owner-record".to_owned()]);
    }
}
