//! Persisting one controller's background ledger without losing another owner's records.
use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;

use super::background::{load_background_state, persist_background_state};
use super::types::PersistedBackgroundRecord;

/// Reloads the shared ledger, merges one controller's snapshot and writes it.
///
/// The caller holds both persistence locks; this function owns the reload,
/// merge and write so they complete as one critical section.
pub(super) async fn persist_background_records(
    workspace_root: &Path,
    owner_session_id: Option<&str>,
    records: Vec<PersistedBackgroundRecord>,
) -> Result<()> {
    if let Some(owner_session_id) = owner_session_id {
        let mut persisted_records = load_background_state(workspace_root).await?.records;
        persisted_records.retain(|record| record.owner_session_id.as_deref() != Some(owner_session_id));
        persisted_records.extend(records);
        persist_background_state(workspace_root, persisted_records).await
    } else {
        // An unscoped controller shares this ledger with the owner-scoped
        // controllers of other sessions and processes, and it loads their
        // records so it can list and route them. Replacing the file with its
        // own snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from the stale
        // copy it holds.
        let persisted_records = load_background_state(workspace_root).await?.records;
        persist_background_state(workspace_root, merge_unscoped_background_records(persisted_records, records)).await
    }
}

/// Merges an unscoped controller's snapshot into the persisted ledger.
///
/// The unscoped controller is not the owner of the owner-scoped records it
/// loads, so it must not replace them with its own copy. Records it does not
/// track are kept exactly as another process last wrote them, and a record
/// both sides hold keeps the copy with the newest `updated_at`: an untouched
/// snapshot equals the copy this controller loaded, while a local refresh or
/// cancellation is newer than it.
fn merge_unscoped_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    // Key on the canonical id: a ledger written by an older build can hold a
    // legacy id for the same owner-scoped record, and the load path
    // canonicalizes in memory. Keying on the raw id would let one record
    // appear twice, or let two owners' records collapse into one entry.
    let mut merged: BTreeMap<String, PersistedBackgroundRecord> = persisted
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            (record.id.clone(), record)
        })
        .collect();
    for mut record in snapshot {
        record.canonicalize_id();
        match merged.get_mut(&record.id) {
            Some(current) if record.is_at_least_as_new_as(current) => *current = record,
            Some(_) => {}
            None => {
                merged.insert(record.id.clone(), record);
            }
        }
    }
    merged.into_values().collect()
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

    proptest! {
        /// The unscoped merge is a per-key join against an independent model.
        ///
        /// The model canonicalizes each generated id from its agent and owner,
        /// keeps the last copy a ledger repeats, prefers the snapshot on a tie,
        /// and keeps every record only one side holds. It never calls the
        /// production id or freshness helpers, so a change to either contract
        /// shows up as a disagreement rather than being mirrored by the test.
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
                expected.insert(merge_canonical_id(record.shape), (record.updated_offset, format!("p{index}")));
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
}
