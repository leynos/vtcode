//! Record-level merges: one controller's snapshot into the shared ledger.
use std::collections::{BTreeMap, BTreeSet};

use super::super::background_ledger::{MigratedBackgroundLaunch, PersistedBackgroundRecord};
use super::launch::{is_migrated_alias, join_by_canonical_id};

/// Merges an unscoped controller's snapshot into the persisted ledger.
///
/// The unscoped controller is not the owner of the owner-scoped records it
/// loads, so it must not replace them with its own copy. This is the unified
/// merge with no removed owner and no removal authority: records it does not
/// track are kept exactly as another process last wrote them, and a record both
/// sides hold keeps the copy with the newest `updated_at`.
pub(super) fn merge_unscoped_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
    expected_stops: &BTreeMap<String, String>,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> Vec<PersistedBackgroundRecord> {
    merge_background_records(persisted, snapshot, None, &BTreeSet::new(), expected_stops, migrated_launches)
}

/// Merges one controller's snapshot into the persisted ledger.
///
/// The snapshot side is canonicalized and joined to the persisted side by
/// canonical id under [`join_by_canonical_id`]'s three-stage rule: within one
/// launch a cancellation outranks an active copy, only the surviving copy of
/// each launch then competes on `updated_at`, and finally an owned copy of a
/// launch outranks an ownerless copy of it. A tie keeps the snapshot, because
/// an untouched snapshot equals the copy this controller loaded while a local
/// refresh or cancellation is newer than it. `expected_stops` names the
/// launches the snapshot is stopping: such a stop never replaces a persisted
/// row of a different exec session under the same canonical id, because that
/// row is a replacement launch another controller wrote after this one loaded
/// its stale copy. A record only one side holds is kept, so no controller
/// discards a record another owner created after it loaded, except that the
/// ownerless copy of a launch another copy records an owner for is dropped.
///
/// Both stages are required. A stop outranks a refresh of the same launch in
/// either direction and even when the refresh was written later, because a
/// refresh advances `updated_at` without changing intent: timestamp order alone
/// would let a stale copy resurrect a cancelled task, or let a newer persisted
/// refresh undo a cancellation this controller had just written. A restart
/// replaces the exec session, so its copy is compared with the stopped launch by
/// timestamp and a newer launch supersedes the one it replaced.
///
/// `removed_owner` names the session whose snapshot is authoritative for its
/// own records. When it is `Some`, only that session's persisted records are
/// canonicalized and joined: a ledger written by an older build can hold a
/// legacy id for the same record, and the load path canonicalizes in memory, so
/// keying on the raw id would let one record appear twice. Every other
/// persisted record is carried verbatim, because another session's record, and
/// an ownerless legacy record, belong to a writer that is not this controller;
/// rewriting their ids would rename records this controller never owned.
///
/// An owner record is removed only when the snapshot no longer carries its
/// canonical id *and* `removable_owner_ids` contains it, so an owner-side
/// removal reaches disk while a record another live controller of the same
/// owner persisted after this controller loaded survives.
///
/// Removal authority is therefore per canonical id, not per launch: the merge
/// cannot tell the record this controller committed from a newer launch
/// another controller wrote under the same id. It does not need to, because a
/// scoped controller never omits an id it has already committed. Its
/// `background_children` only gains entries in production: the only mutation
/// that clears the map, the `std::mem::take` in
/// `SubagentController::adopt_persisted_background_owner`, reinserts every
/// record, and rekeys only an ownerless row whose `parent_session_id` the
/// ledger now records under the owner — rows a scoped controller does not
/// hold, so its own ids keep the same keys. No other `remove`, `retain` or
/// `clear` touches the map. A controller therefore still holds every id it
/// committed at the moment another controller writes a replacement under one,
/// so that snapshot carries the id and the replacement survives the join. The
/// retain arm that would drop a newer launch under an authorised id is
/// unreachable in production and exists only to keep the branch total.
pub(super) fn merge_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
    removed_owner: Option<&str>,
    removable_owner_ids: &BTreeSet<String>,
    expected_stops: &BTreeMap<String, String>,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> Vec<PersistedBackgroundRecord> {
    let mut carried_verbatim: Vec<PersistedBackgroundRecord> = Vec::new();
    let mut owned: Vec<PersistedBackgroundRecord> = Vec::new();
    for record in persisted {
        // A known stale alias is dropped whichever scope last held it: the
        // migration evidence names the launch by its ownerless id and exec
        // session, not by the writer. Another owner's live record is still
        // carried verbatim, because rewriting it would rename a record this
        // controller never owned.
        if is_migrated_alias(&record, migrated_launches) {
            continue;
        }
        match removed_owner {
            Some(owner) if record.owner_session_id.as_deref() != Some(owner) => carried_verbatim.push(record),
            _ => owned.push(record),
        }
    }
    // Canonicalize once so the removal check and the join agree on the key.
    let snapshot = snapshot
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            record
        })
        .collect::<Vec<_>>();
    let snapshot_ids = snapshot.iter().map(|record| record.id.clone()).collect::<BTreeSet<_>>();
    let mut merged = join_by_canonical_id(owned, snapshot, expected_stops, migrated_launches);
    if let Some(owner) = removed_owner {
        merged.retain(|id, record| {
            record.owner_session_id.as_deref() != Some(owner)
                || snapshot_ids.contains(id)
                || !removable_owner_ids.contains(id)
        });
    }
    carried_verbatim.extend(merged.into_values());
    carried_verbatim
}
