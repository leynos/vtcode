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
///
/// `expected_stops` names the launches the snapshot is stopping, keyed by
/// canonical id and carrying the exec session each stop applies to. A stop
/// only governs the launch it names, so the merge keeps a persisted row for a
/// different exec session under the same id: that row is a replacement launch,
/// not a stale copy of the stopped one.
pub(super) async fn persist_background_records(
    workspace_root: &Path,
    owner_session_id: Option<&str>,
    records: Vec<PersistedBackgroundRecord>,
    removable_owner_ids: &BTreeSet<String>,
    expected_stops: &BTreeMap<String, String>,
) -> Result<()> {
    // A test parks a save here so it can prove reuse waited for the owed write
    // instead of answering from the in-memory record; see the
    // `ledger_write_checkpoints` test module below.
    #[cfg(all(test, unix))]
    ledger_write_checkpoints::pass(workspace_root).await;
    let persisted_records = load_background_state(workspace_root).await?.records;
    let merged = match owner_session_id {
        // An owner-scoped controller supplies the authoritative set of records
        // its own session still tracks, so a record it dropped is removed from
        // the ledger. It must not replace the copies it holds: an unscoped
        // controller shares the ledger and can cancel one of this owner's
        // tasks, persisting a cancellation newer than the stale copy this
        // controller loaded.
        Some(owner_session_id) => merge_background_records(
            persisted_records,
            records,
            Some(owner_session_id),
            removable_owner_ids,
            expected_stops,
        ),
        // An unscoped controller loads other owners' records so it can list and
        // route them, and it owns none of them. Replacing the file with its own
        // snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from its stale copy.
        None => merge_unscoped_background_records(persisted_records, records, expected_stops),
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
    join_by_canonical_id(records, std::iter::empty(), &BTreeMap::new())
}

/// One copy of a canonical record together with the merge side it came from.
///
/// The provenance is what lets a tie keep the snapshot copy, and what keeps a
/// stopped snapshot copy from erasing a different launch that another
/// controller has since persisted under the same canonical id.
struct LaunchCandidate {
    record: PersistedBackgroundRecord,
    /// Whether this copy arrived on the snapshot side of the merge.
    from_snapshot: bool,
}

impl LaunchCandidate {
    /// Whether `candidate` replaces `chosen` as `id`'s survivor.
    ///
    /// Freshness decides by `updated_at`, and a tie keeps the snapshot copy:
    /// an untouched snapshot equals the copy this controller loaded while any
    /// local change is newer than it. Without the provenance a tie fell to the
    /// lexicographically later exec session, which is not the documented rule.
    ///
    /// A stop only applies to the launch it names, so a stopped snapshot copy
    /// never displaces a persisted row that names a different exec session
    /// under the same canonical id, whatever their timestamps. That row is a
    /// replacement launch rather than a stale copy of the stopped one, and
    /// deleting it would erase a process no other controller can restore. The
    /// caller applies the answer in both directions, which makes the rule
    /// independent of the order the two copies are reduced in.
    fn candidate_outranks(
        id: &str,
        chosen: &Self,
        candidate: &Self,
        expected_stops: &BTreeMap<String, String>,
    ) -> bool {
        if stop_does_not_name_other_launch(id, chosen, candidate, expected_stops) {
            return true;
        }
        if stop_does_not_name_other_launch(id, candidate, chosen, expected_stops) {
            return false;
        }
        // Equal timestamps are a tie in both directions, so the snapshot
        // provenance decides it rather than the exec session's map order.
        candidate.record.is_at_least_as_new_as(&chosen.record)
            && (!chosen.record.is_at_least_as_new_as(&candidate.record)
                || (candidate.from_snapshot && !chosen.from_snapshot))
    }
}

/// Whether `stop` is an expected stop that must not displace `other`.
///
/// `expected_stops` names the launches this save is stopping and the exec
/// session each stop applies to. When `other` is a persisted copy of a
/// different exec session under the same canonical id, it is a replacement
/// launch rather than a stale copy of the stopped one, so the stop yields to
/// it.
fn stop_does_not_name_other_launch(
    id: &str,
    stop: &LaunchCandidate,
    other: &LaunchCandidate,
    expected_stops: &BTreeMap<String, String>,
) -> bool {
    expected_stops
        .get(id)
        .is_some_and(|exec_session_id| exec_session_id == &stop.record.exec_session_id)
        && !other.from_snapshot
        && stop.record.exec_session_id != other.record.exec_session_id
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
/// of the stopped one. A tie keeps the snapshot copy over the persisted copy
/// it equals. `expected_stops` names the launches the snapshot is stopping, so
/// a stop may only replace a persisted row that names its own exec session: a
/// stale stop cannot erase a replacement launch another controller has already
/// written under the same canonical id.
///
/// A copy that records an owner outranks an ownerless copy of the same
/// launch, whatever their timestamps. Ownerless and owned copies of one launch
/// canonicalize to different keys, because the key is derived from the owner,
/// so this cannot be decided inside a group: an ownerless copy is dropped when
/// any input copy of the same launch (`agent_name` and `exec_session_id`)
/// records an owner. That is what stops a controller that loaded a row before
/// its owner was recorded — the ACP launch workspace, which restores ownerless
/// rows before any session resumes — from saving the ownerless copy back
/// alongside the migrated record and leaving a restorable duplicate of a
/// launch a session now owns.
///
/// The ownership evidence is taken from every input copy, not only from the
/// launches that survived the reduction. A newer owned launch supersedes the
/// older owned launch it replaces, so the older launch may have no owned
/// survivor for an ownerless copy to match against; reading only the survivors
/// would let that ownerless copy survive beside the replacement. Dropping the
/// ownerless copy is the conservative choice whenever an owned copy exists in
/// the merge: the owned copy is the one every scope agrees on, an ownerless
/// copy in memory is not authoritatively ownerless, and dropping it loses at
/// most a refresh that cannot be persisted deterministically.
///
/// Reducing per launch before comparing launches is what makes the choice
/// independent of input order. A single comparator over every copy of the key
/// is cyclic because cancellation dominance and timestamp order disagree: with
/// an active `exec-1` copy at t3, a cancelled `exec-1` copy at t1 and an active
/// `exec-2` copy at t2, file order `[cancelled, exec-2, active]` keeps the
/// active `exec-1` copy and resurrects the stopped launch, while
/// `[active, cancelled, exec-2]` keeps `exec-2`. The ownership filter is also
/// independent of input order: it depends only on which launches the inputs
/// attribute to an owner, not on where their copies appeared or which of them
/// the reduction kept.
fn join_by_canonical_id(
    persisted: impl IntoIterator<Item = PersistedBackgroundRecord>,
    snapshot: impl IntoIterator<Item = PersistedBackgroundRecord>,
    expected_stops: &BTreeMap<String, String>,
) -> BTreeMap<String, PersistedBackgroundRecord> {
    type LaunchCopies = BTreeMap<String, LaunchCandidate>;
    let mut launches: BTreeMap<String, LaunchCopies> = BTreeMap::new();
    // Every launch an input copy attributes to an owner. Collected before the
    // reduction because a newer owned launch supersedes an older owned one, so
    // the superseded launch's ownerless copy has no owned survivor to match.
    let mut owned_launches = BTreeSet::new();
    let persisted = persisted
        .into_iter()
        .map(|record| LaunchCandidate { record, from_snapshot: false });
    let snapshot = snapshot
        .into_iter()
        .map(|record| LaunchCandidate { record, from_snapshot: true });
    for mut candidate in persisted.chain(snapshot) {
        candidate.record.canonicalize_id();
        if candidate.record.owner_session_id.is_some() {
            owned_launches.insert(ownerless_launch(&candidate.record));
        }
        let id = candidate.record.id.clone();
        let exec_session_id = candidate.record.exec_session_id.clone();
        let sessions = launches.entry(id).or_default();
        match sessions.entry(exec_session_id) {
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                // Cancellation dominance decides within one launch; freshness
                // only decides between launches, or between two copies that
                // agree on intent. A stopped current copy must survive a newer
                // active copy, so it is checked before freshness. A tie keeps
                // the later entry, which is the snapshot copy when the two
                // sides agree.
                let replace = {
                    let current = entry.get();
                    !current.record.cancellation_outranks(&candidate.record)
                        && (candidate.record.cancellation_outranks(&current.record)
                            || candidate.record.is_at_least_as_new_as(&current.record))
                };
                if replace {
                    entry.insert(candidate);
                }
            }
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(candidate);
            }
        }
    }
    let survivors = launches
        .into_iter()
        .filter_map(|(id, sessions)| {
            // An id is only inserted into `launches` together with at least one
            // launch copy, so the empty group this skips is unreachable. It is
            // still skipped rather than unwrapped: a merge of persisted state
            // must never panic, and dropping an id it cannot represent loses at
            // most one launch record.
            sessions
                .into_values()
                .reduce(|chosen, candidate| {
                    if LaunchCandidate::candidate_outranks(&id, &chosen, &candidate, expected_stops) {
                        candidate
                    } else {
                        chosen
                    }
                })
                .map(|chosen| (id, chosen.record))
        })
        .collect::<BTreeMap<_, _>>();
    survivors
        .into_iter()
        .filter(|(_, record)| record.owner_session_id.is_some() || !owned_launches.contains(&ownerless_launch(record)))
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
    expected_stops: &BTreeMap<String, String>,
) -> Vec<PersistedBackgroundRecord> {
    merge_background_records(persisted, snapshot, None, &BTreeSet::new(), expected_stops)
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
fn merge_background_records(
    persisted: Vec<PersistedBackgroundRecord>,
    snapshot: Vec<PersistedBackgroundRecord>,
    removed_owner: Option<&str>,
    removable_owner_ids: &BTreeSet<String>,
    expected_stops: &BTreeMap<String, String>,
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
    let mut merged = join_by_canonical_id(owned, snapshot, expected_stops);
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
    /// `owner_session_id` *and* names the same exec session *and* the copy's
    /// own verified `parent_session_id` is exactly that owner. The exec
    /// session is part of the identity: a canonical id alone also matches a
    /// different launch of the same agent that the session has since
    /// persisted, and rekeying the stale copy onto it would overwrite the
    /// launch the session actually owns on this controller's next save. A
    /// failed migration therefore adopts nothing: the ledger still names the
    /// row ownerless, and this controller must not invent an owner the
    /// migration did not write. Only the record's owner changes; its launch
    /// and every other field stay as loaded, and it is rekeyed to the
    /// canonical id the new owner gives it.
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
                (record.id, record.exec_session_id)
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
                && owned_launches.contains(&(
                    super::background::background_record_id_for_owner(
                        record.agent_name.as_str(),
                        Some(owner_session_id),
                    ),
                    record.exec_session_id.clone(),
                ));
            if launched_here {
                adopted += 1;
                let previous_id = record.id.clone();
                record.owner_session_id = Some(owner_session_id.to_string());
                record.id = super::background::background_record_id_for_owner(
                    record.agent_name.as_str(),
                    Some(owner_session_id),
                );
                // A close this launch still owes was registered under the id it
                // had before adoption. Move it with the launch, or the retry
                // would miss it and discharge an obligation it never attempted.
                super::controller_background_ops::rekey_pending_background_stop_close(
                    &self.state,
                    previous_id.as_str(),
                    record.id.as_str(),
                    record.exec_session_id.as_str(),
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

/// Test-only rendezvous inside a background-ledger save.
///
/// A launch commits its record in memory before the ledger write, and reuse
/// must not answer from that record until the owed write has run. Parking the
/// save makes the ordering observable: a test can prove reuse has not returned
/// before the write reached the ledger, instead of treating a timeout that did
/// not elapse as evidence. The module mirrors
/// `controller_tree::attachment_checkpoints`: a checkpoint is keyed by the
/// workspace whose save it holds, and a dropped test's entry is pruned before
/// its path can be reused.
#[cfg(all(test, unix))]
pub(crate) mod ledger_write_checkpoints {
    use super::*;
    use std::path::PathBuf;

    struct Checkpoint {
        workspace_root: PathBuf,
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }

    fn checkpoints() -> &'static Mutex<Vec<Checkpoint>> {
        static CHECKPOINTS: OnceLock<Mutex<Vec<Checkpoint>>> = OnceLock::new();
        CHECKPOINTS.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Arms a checkpoint that parks the next save of `workspace_root`.
    ///
    /// Returns the signal that a save reached the checkpoint and the signal
    /// that releases it.
    pub(crate) fn arm(workspace_root: &Path) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        registry.retain(|checkpoint| Arc::strong_count(&checkpoint.entered) > 1);
        registry.push(Checkpoint {
            workspace_root: workspace_root.to_path_buf(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        (entered, release)
    }

    /// Parks here when `workspace_root` has an armed checkpoint.
    pub(super) async fn pass(workspace_root: &Path) {
        let checkpoint = {
            let mut registry = checkpoints().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            registry.retain(|checkpoint| Arc::strong_count(&checkpoint.entered) > 1);
            registry
                .iter()
                .position(|checkpoint| checkpoint.workspace_root.as_path() == workspace_root)
                .map(|index| registry.remove(index))
        };
        let Some(checkpoint) = checkpoint else {
            return;
        };
        checkpoint.entered.notify_one();
        checkpoint.release.notified().await;
    }
}

#[cfg(test)]
mod ownership_supersession_tests {
    //! Pins that a superseded owned launch leaves no ownerless copy behind.

    use super::super::background::background_record_id_for_owner;
    use super::*;

    /// Builds one persisted record with the canonical id for its agent/owner.
    fn persisted(
        agent: &str,
        owner_session_id: Option<&str>,
        exec_session_id: &str,
        updated_at: &str,
    ) -> PersistedBackgroundRecord {
        serde_json::from_value(serde_json::json!({
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

        let merged = join_by_canonical_id(ledger, snapshot, &BTreeMap::new());

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
}

#[cfg(test)]
mod tests;
