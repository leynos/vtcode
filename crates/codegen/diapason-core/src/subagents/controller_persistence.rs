//! Persisting one controller's background ledger without losing another owner's records.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::Result;
use tokio::sync::RwLock;

use super::background::{load_background_state, persist_background_state};
use super::types::{ControllerState, PersistedBackgroundRecord};

/// Reloads the shared ledger, merges one controller's snapshot and writes it.
///
/// The caller holds both persistence locks; this function owns the reload,
/// merge and write so they complete as one critical section.
///
/// The ledger is shared with the owner-scoped controllers of other sessions
/// and processes, so a save is a merge rather than a replacement. A record
/// another owner created after this controller loaded survives, and a record
/// both sides hold keeps the copy with the newest `updated_at`.
///
/// `removable_owner_ids` names the canonical ids this controller has already
/// committed for its owner. An owner-scoped save removes a record its session
/// no longer carries only when that id is in the set, so an owner-side removal
/// still reaches disk without letting a stale snapshot delete a record another
/// live controller of the same owner persisted.
pub(super) async fn persist_background_records(
    workspace_root: &Path,
    owner_session_id: Option<&str>,
    records: Vec<PersistedBackgroundRecord>,
    removable_owner_ids: &BTreeSet<String>,
) -> Result<()> {
    let persisted_records = load_background_state(workspace_root).await?.records;
    let merged = match owner_session_id {
        // An owner-scoped controller supplies the authoritative set of records
        // its own session still tracks, so a record it dropped is removed from
        // the ledger. It must not replace the copies it holds: an unscoped
        // controller shares the ledger and can cancel one of this owner's
        // tasks, persisting a cancellation newer than the stale copy this
        // controller loaded.
        Some(owner_session_id) => {
            merge_background_records(persisted_records, records, Some(owner_session_id), removable_owner_ids)
        }
        // An unscoped controller loads other owners' records so it can list and
        // route them, and it owns none of them. Replacing the file with its own
        // snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from its stale copy.
        None => merge_unscoped_background_records(persisted_records, records),
    };
    persist_background_state(workspace_root, merged).await
}

/// The owner-scoped ids one controller tree has already committed.
///
/// An owner-scoped save removes a persisted owner record whose id its snapshot
/// no longer carries, which is what propagates an owner-side removal. The
/// snapshot is not authoritative for a record the controller never loaded: a
/// second live controller of the same owner can persist one after this
/// controller loaded, and the ledger schema records no writer. Each controller
/// tree therefore remembers the ids it has committed, and only those may be
/// removed. Entries are keyed by the tree's shared state allocation, so every
/// clone observes one history, and a dropped tree's entry is pruned on the next
/// access.
fn committed_owner_ids() -> &'static Mutex<Vec<CommittedOwnerIds>> {
    static COMMITTED: OnceLock<Mutex<Vec<CommittedOwnerIds>>> = OnceLock::new();
    COMMITTED.get_or_init(|| Mutex::new(Vec::new()))
}

/// One controller tree's committed id history.
struct CommittedOwnerIds {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    ids: BTreeSet<String>,
}

/// Applies `update` to the history of the controller tree that owns `state`.
fn with_committed_owner_ids<R>(
    state: &Arc<RwLock<ControllerState>>,
    update: impl FnOnce(&mut BTreeSet<String>) -> R,
) -> R {
    let mut registry = committed_owner_ids().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|entry| entry.state.strong_count() > 0);
    if let Some(entry) = registry
        .iter_mut()
        .find(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
    {
        return update(&mut entry.ids);
    }
    let mut ids = BTreeSet::new();
    let result = update(&mut ids);
    registry.push(CommittedOwnerIds { state: Arc::downgrade(state), ids });
    result
}

/// Returns the owner-scoped ids this controller tree may remove on its next save.
pub(super) fn removable_owner_ids(state: &Arc<RwLock<ControllerState>>) -> BTreeSet<String> {
    with_committed_owner_ids(state, |committed| committed.clone())
}

/// Records the owner-scoped ids this controller tree just committed.
///
/// Called only after the merge and write succeeded, so a failed save cannot
/// grant authority to remove a record this controller did not actually own.
pub(super) fn record_committed_owner_ids(state: &Arc<RwLock<ControllerState>>, ids: impl IntoIterator<Item = String>) {
    with_committed_owner_ids(state, |committed| committed.extend(ids));
}

/// Collects persisted records by canonical id, keeping the freshest copy.
///
/// Several entries can canonicalize to one key when a legacy ledger holds both
/// a pre-canonicalization id and its current form. A stop outranks a newer copy
/// of the same launch in either direction: collapsing must not discard a
/// cancellation in favour of an active copy that only refreshed `updated_at`,
/// or the later merge would never see the stop. For copies from different
/// launches the newest `updated_at` wins and a tie keeps the later entry, so
/// neither the ledger's file order nor the order a controller happens to
/// rebuild its map in decides which copy of a record survives. The load path
/// and the save merge share this rule.
pub(super) fn collect_by_canonical_id(
    records: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    let mut collected: BTreeMap<String, PersistedBackgroundRecord> = BTreeMap::new();
    for mut record in records {
        record.canonicalize_id();
        match collected.get_mut(&record.id) {
            Some(current) if current.cancellation_outranks(&record) => {}
            Some(current) if record.cancellation_outranks(current) => *current = record,
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
/// merge with no removed owner and no removal authority: records it does not
/// track are kept exactly as another process last wrote them, and a record both
/// sides hold keeps the copy with the newest `updated_at`.
fn merge_unscoped_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
) -> Vec<PersistedBackgroundRecord> {
    merge_background_records(persisted, snapshot, None, &BTreeSet::new())
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
/// A stop outranks the other copy when the two name the same exec session, in
/// either direction and even if the other copy was written later: a refresh
/// advances `updated_at` without changing intent, so timestamp order alone
/// would let a stale copy resurrect a cancelled task, or let a newer persisted
/// refresh undo a cancellation this controller had just written. A restart
/// replaces the exec session, so it is never mistaken for that stale copy.
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
fn merge_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
    removed_owner: Option<&str>,
    removable_owner_ids: &BTreeSet<String>,
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
            Some(current) if record.cancellation_outranks(current) => *current = record,
            Some(current) if record.is_at_least_as_new_as(current) => *current = record,
            Some(_) => {}
            None => {
                merged.insert(id.clone(), record);
            }
        }
        snapshot_ids.insert(id);
    }
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

#[cfg(test)]
mod tests;
