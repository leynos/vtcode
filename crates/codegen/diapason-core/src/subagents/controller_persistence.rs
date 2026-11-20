//! Persisting one controller's background ledger without losing another owner's records.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::Result;
use tokio::sync::RwLock;

use super::SubagentController;
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
    /// The canonical ids this controller tree included in a save that merged
    /// and persisted.
    ///
    /// The set only grows. An id is added once its write succeeded and is never
    /// removed for the life of the tree, so a later save cannot lose authority
    /// a failed write never granted. Only these ids may be removed from the
    /// ledger; every other record belongs to a writer this controller has no
    /// evidence about.
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
/// A scoped controller's later snapshots always carry these ids again, because
/// its `background_children` never loses an entry; see
/// [`merge_background_records`] for the invariant that makes that true.
pub(super) fn record_committed_owner_ids(state: &Arc<RwLock<ControllerState>>, ids: impl IntoIterator<Item = String>) {
    with_committed_owner_ids(state, |committed| committed.extend(ids));
}

/// Collects persisted records by canonical id, keeping one copy per launch.
///
/// Several entries can canonicalize to one key when a legacy ledger holds both
/// a pre-canonicalization id and its current form. The load path and the save
/// merge share [`join_by_canonical_id`]'s reduction rule, so the collapse does
/// not depend on the ledger's file order or the order a controller happens to
/// rebuild its map in. The ownership precedence applies here too, so a ledger
/// that holds both an ownerless and an owned copy of one launch loads only the
/// owned copy, which is the one that may be restored.
pub(super) fn collect_by_canonical_id(
    records: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    join_by_canonical_id(records, std::iter::empty())
}

/// Joins copies of each canonical record by launch, then by ownership and
/// freshness.
///
/// Copies are grouped first by canonical id and `exec_session_id`. Within one
/// launch a cancellation outranks an active copy in either direction, so a
/// refresh that only advanced `updated_at` cannot resurrect a stopped task.
/// Only the surviving copy of each launch is then compared by `updated_at`,
/// which is what lets an explicit restart supersede the launch it replaced: a
/// restart writes a new exec session, so it is never mistaken for a stale copy
/// of the stopped one. A tie keeps the later entry, so a snapshot copy wins
/// over the persisted copy it equals.
///
/// A copy that records an owner then outranks an ownerless copy of the same
/// launch, whatever their timestamps. Ownerless and owned copies of one launch
/// canonicalize to different keys, because the key is derived from the owner,
/// so this cannot be decided inside a group: after each key's launch is
/// reduced, an ownerless survivor is dropped when an owned survivor names the
/// same launch (`agent_name` and `exec_session_id`). That is what stops a
/// controller that loaded a row before its owner was recorded — the ACP launch
/// workspace, which restores ownerless rows before any session resumes — from
/// saving the ownerless copy back alongside the migrated record and leaving a
/// restorable duplicate of a launch a session now owns. Dropping the ownerless
/// copy is the conservative choice whenever the two exist at once: the owned
/// copy is the one every scope agrees on, an ownerless copy in memory is not
/// authoritatively ownerless, and dropping it loses at most a refresh that
/// cannot be persisted deterministically.
///
/// Reducing per launch before comparing launches is what makes the choice
/// independent of input order. A single comparator over every copy of the key
/// is cyclic because cancellation dominance and timestamp order disagree: with
/// an active `exec-1` copy at t3, a cancelled `exec-1` copy at t1 and an active
/// `exec-2` copy at t2, file order `[cancelled, exec-2, active]` keeps the
/// active `exec-1` copy and resurrects the stopped launch, while
/// `[active, cancelled, exec-2]` keeps `exec-2`. The ownership filter runs on
/// the reduced survivors for the same reason: it depends only on which
/// launches survived, not on where their copies appeared.
fn join_by_canonical_id(
    persisted: impl IntoIterator<Item = PersistedBackgroundRecord>,
    snapshot: impl IntoIterator<Item = PersistedBackgroundRecord>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    type LaunchCopies = BTreeMap<String, PersistedBackgroundRecord>;
    let mut launches: BTreeMap<String, LaunchCopies> = BTreeMap::new();
    for mut record in persisted.into_iter().chain(snapshot) {
        record.canonicalize_id();
        let id = record.id.clone();
        let exec_session_id = record.exec_session_id.clone();
        let sessions = launches.entry(id).or_default();
        match sessions.entry(exec_session_id) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Cancellation dominance decides within one launch; freshness
                // only decides between launches, or between two copies that
                // agree on intent. A stopped current copy must survive a newer
                // active copy, so it is checked before freshness.
                let replace = {
                    let current = entry.get();
                    !current.cancellation_outranks(&record)
                        && (record.cancellation_outranks(current) || record.is_at_least_as_new_as(current))
                };
                if replace {
                    entry.insert(record);
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(record);
            }
        }
    }
    let survivors = launches
        .into_iter()
        .map(|(id, sessions)| {
            let mut representatives = sessions.into_values();
            let mut chosen = representatives
                .next()
                .expect("a canonical id is recorded with at least one launch");
            for candidate in representatives {
                if candidate.is_at_least_as_new_as(&chosen) {
                    chosen = candidate;
                }
            }
            (id, chosen)
        })
        .collect::<BTreeMap<_, _>>();
    let unowned = survivors
        .values()
        .filter(|record| record.owner_session_id.is_some())
        .map(ownerless_launch)
        .collect::<BTreeSet<_>>();
    survivors
        .into_iter()
        .filter(|(_, record)| record.owner_session_id.is_some() || !unowned.contains(&ownerless_launch(record)))
        .collect()
}

/// Names the launch a record is a copy of, ignoring its owner.
///
/// The ownerless base id of a launch is the key its ownerless copy carries, so
/// an owned and an ownerless copy of one launch compare equal once the owner is
/// cleared and the id recomputed.
fn ownerless_launch(record: &PersistedBackgroundRecord) -> (String, String) {
    let mut base = record.clone();
    base.owner_session_id = None;
    base.canonicalize_id();
    (base.id, base.exec_session_id)
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
/// canonical id under [`join_by_canonical_id`]'s three-stage rule: within one
/// launch a cancellation outranks an active copy, only the surviving copy of
/// each launch then competes on `updated_at`, and finally an owned copy of a
/// launch outranks an ownerless copy of it. A tie keeps the snapshot, because
/// an untouched snapshot equals the copy this controller loaded while a local
/// refresh or cancellation is newer than it. A record only one side holds is
/// kept, so no controller discards a record another owner created after it
/// loaded, except that the ownerless copy of a launch another copy records an
/// owner for is dropped.
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
    // Canonicalize once so the removal check and the join agree on the key.
    let snapshot = snapshot
        .into_iter()
        .map(|mut record| {
            record.canonicalize_id();
            record
        })
        .collect::<Vec<_>>();
    let snapshot_ids = snapshot.iter().map(|record| record.id.clone()).collect::<BTreeSet<_>>();
    let mut merged = join_by_canonical_id(owned, snapshot);
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

impl SubagentController {
    /// Gives this controller's in-memory copies of `owner_session_id`'s launches
    /// that owner, matching the ledger a resumed session has migrated.
    ///
    /// A controller that loaded the ledger before the migration — the ACP
    /// launch workspace, which is built before any session attaches — holds
    /// ownerless copies of rows a resumed session now owns. Its in-memory
    /// copies would otherwise be restored as its own work, and its next save
    /// would write them back as ownerless duplicates beside the owned records.
    ///
    /// A copy is adopted only when the ledger now records that launch under
    /// `owner_session_id` *and* the copy's own verified `parent_session_id` is
    /// exactly that owner. A failed migration therefore adopts nothing: the
    /// ledger still names the row ownerless, and this controller must not
    /// invent an owner the migration did not write. Only the record's owner
    /// changes; its launch and every other field stay as loaded, and it is
    /// rekeyed to the canonical id the new owner gives it.
    ///
    /// Returns the number of records adopted. A `managed_background_runtime`
    /// controller owns nothing and adopts nothing, and an empty owner names
    /// none.
    pub async fn adopt_persisted_background_owner(&self, owner_session_id: &str) -> Result<usize> {
        if owner_session_id.is_empty() || self.config.managed_background_runtime {
            return Ok(0);
        }
        let owned_launches = load_background_state(&self.config.workspace_root)
            .await?
            .records
            .into_iter()
            .filter(|record| record.owner_session_id.as_deref() == Some(owner_session_id))
            .map(|mut record| {
                record.canonicalize_id();
                record.id
            })
            .collect::<BTreeSet<_>>();
        if owned_launches.is_empty() {
            return Ok(0);
        }
        let mut state = self.state.write().await;
        let mut rekeyed = BTreeMap::new();
        let mut adopted = 0;
        for mut record in std::mem::take(&mut state.background_children).into_values() {
            let launched_here = record.owner_session_id.is_none()
                && record.parent_session_id == owner_session_id
                && owned_launches.contains(&super::background::background_record_id_for_owner(
                    record.agent_name.as_str(),
                    Some(owner_session_id),
                ));
            if launched_here {
                adopted += 1;
                record.owner_session_id = Some(owner_session_id.to_string());
                record.id = super::background::background_record_id_for_owner(
                    record.agent_name.as_str(),
                    Some(owner_session_id),
                );
            }
            // Every later lookup keys on the canonical id, so a record that
            // changed owner is rekeyed to the one its new owner gives it.
            rekeyed.insert(record.id.clone(), record);
        }
        state.background_children = rekeyed;
        Ok(adopted)
    }

    /// Persists this controller's background records into the shared ledger.
    ///
    /// The ordinary save follows a launch, a refresh or a cancellation; this
    /// exposes it for a caller that has only reconciled its in-memory copies
    /// and must make that state durable, such as a resumed session adopting
    /// rows a launch controller still held ownerless. It is a no-op for a
    /// controller that owns no background work and is skipped once shutdown has
    /// been requested.
    pub async fn save_background_ledger(&self) -> Result<()> {
        self.save_background_state().await
    }
}

#[cfg(test)]
mod tests;
