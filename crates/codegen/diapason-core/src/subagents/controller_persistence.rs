//! Persisting one controller's background ledger without losing another owner's records.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use anyhow::Result;
use tokio::sync::RwLock;

mod launch;
mod merge;

pub(super) use self::launch::collect_by_canonical_id;
use self::merge::{merge_background_records, merge_unscoped_background_records};
use super::SubagentController;
use super::background::load_background_state;
use super::background_ledger::{
    MigratedBackgroundLaunch, PersistedBackgroundRecord, load_background_ledger, persist_background_ledger,
};
use super::types::ControllerState;

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
///
/// The save also maintains the ledger's durable migration evidence: every
/// owned launch it reads or writes is recorded by ownerless id and exec
/// session, so a workspace copy cached before a migration is still recognised
/// after an owned replacement supersedes the migrated row. The evidence is
/// kept beside the records in the ledger file, which the ACP owner migration
/// rewrites as generic JSON and therefore preserves.
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
    let ledger = load_background_ledger(workspace_root).await?;
    let mut migrated_launches = ledger.migrated_launches;
    // Every owned copy names a launch a migration has already attributed to a
    // session, whichever process performed it. Recording it here keeps the
    // evidence after the owned row is itself replaced, which is the only moment
    // an ownerless alias of that launch can reappear unnoticed.
    for record in &ledger.records {
        note_migrated_launch(&mut migrated_launches, record);
    }
    let merged = match owner_session_id {
        // An owner-scoped controller supplies the authoritative set of records
        // its own session still tracks, so a record it dropped is removed from
        // the ledger. It must not replace the copies it holds: an unscoped
        // controller shares the ledger and can cancel one of this owner's
        // tasks, persisting a cancellation newer than the stale copy this
        // controller loaded.
        Some(owner_session_id) => merge_background_records(
            ledger.records,
            records,
            Some(owner_session_id),
            removable_owner_ids,
            expected_stops,
            &migrated_launches,
        ),
        // An unscoped controller loads other owners' records so it can list and
        // route them, and it owns none of them. Replacing the file with its own
        // snapshot would delete a record another owner created after this
        // controller loaded, or restore that owner's record from its stale copy.
        None => merge_unscoped_background_records(ledger.records, records, expected_stops, &migrated_launches),
    };
    for record in &merged {
        note_migrated_launch(&mut migrated_launches, record);
    }
    persist_background_ledger(workspace_root, merged, &migrated_launches).await
}

/// Adds one owned launch to the durable migration evidence.
///
/// The evidence names the launch by the ownerless id its stale copy still
/// carries and by its exec session, so an independent ownerless launch of the
/// same agent is not suppressed.
fn note_migrated_launch(evidence: &mut Vec<MigratedBackgroundLaunch>, record: &PersistedBackgroundRecord) {
    if record.owner_session_id.is_none() {
        return;
    }
    evidence.push(MigratedBackgroundLaunch {
        base_id: record.ownerless_base_id(),
        exec_session_id: record.exec_session_id.clone(),
    });
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
mod tests;
