//! Owner migration and rollback for background sub-agents restored by ACP.
//!
//! Two properties keep a resumed ACP session from ending up with two processes
//! for one background record.
//!
//! A ledger row written before background ownership existed names the ACP
//! session that launched it in `parent_session_id` but carries no
//! `owner_session_id`. The workspace controller restores only ownerless rows,
//! so without a migration it would relaunch such a row as its own work, and the
//! resumed session's controller would never adopt it.
//! A restore that fails after spawning leaves the child running under a new
//! exec session nothing persisted, because the save that records the launch is
//! the step that failed. A retried attach cannot see that launch in the ledger,
//! so it spawns a second process for the same record while the first keeps
//! running. A failed restore therefore rolls back exactly the launches it made
//! whose identity did not reach the ledger; a persisted launch, and a restore
//! that succeeded, are left running.
//!
//! Rollback selects only a record the launch commit marked `Running`. A spawn
//! that failed keeps its newly assigned exec session and `desired_enabled` but
//! is settled `Error`; it never had a process, so force-cancelling it would
//! permanently clear the desire to run a task nothing started.
//!
//! Rollback also requires a ledger it can verify. A ledger that cannot be read
//! or parsed may still record a launch this pass cannot see, so no rollback is
//! attempted; the in-memory controller remains the record of the live process.

use anyhow::Context;
use diapason_core::subagents::{BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentController, TaskOwner};
use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use tokio::fs;

#[cfg(test)]
mod tests;

/// Resolves the background ledger path for a workspace root.
///
/// Core keeps its record type and loader private to the subagent module, so the
/// migration reads and rewrites the documented ledger schema directly. The path
/// derives from the same workspace-directory helper core uses, so a workspace
/// directory override cannot desynchronise the two.
fn ledger_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.json")
}

/// Gives ownerless ledger rows to the ACP session that launched them.
///
/// A row is migrated only when its verified `parent_session_id` is exactly
/// `owner_session_id` and it carries no owner, so one session can never take
/// over another session's record. The row's id and every other field are left
/// untouched: core canonicalises the id on load, so the stored legacy id keeps
/// addressing the same record.
///
/// Returns the number of rows migrated. A missing ledger is a no-op, and a
/// second call migrates nothing. The rewrite holds the ledger's inter-process
/// lock, so it cannot race a save from another process.
pub(super) async fn migrate_legacy_owner_rows(workspace_root: &Path, owner_session_id: &str) -> anyhow::Result<usize> {
    if owner_session_id.is_empty() {
        return Ok(0);
    }
    let _ledger_guard = diapason_core::subagents::lock_background_ledger(workspace_root)
        .await
        .context("Failed to lock the background subagent ledger for owner migration")?;
    let path = ledger_path(workspace_root);
    let Some(mut state) = read_ledger_state(&path).await? else {
        return Ok(0);
    };
    let migrated = assign_owner_to_launched_rows(&mut state, owner_session_id);
    if migrated == 0 {
        return Ok(0);
    }
    write_migrated_ledger_state(&path, &state).await?;
    Ok(migrated)
}

/// Reads the ledger document, treating a missing ledger as nothing to migrate.
async fn read_ledger_state(path: &Path) -> anyhow::Result<Option<serde_json::Value>> {
    let raw = match fs::read_to_string(path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    let state = serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(Some(state))
}

/// Gives every ownerless row this session launched to `owner_session_id`.
///
/// Returns the number of rows migrated. A row launched by another session, and
/// a row that already carries an owner, are left untouched.
fn assign_owner_to_launched_rows(state: &mut serde_json::Value, owner_session_id: &str) -> usize {
    let Some(records) = state.get_mut("records").and_then(serde_json::Value::as_array_mut) else {
        return 0;
    };
    let mut migrated = 0;
    for record in records {
        let Some(fields) = record.as_object_mut() else {
            continue;
        };
        let launched_here =
            fields.get("parent_session_id").and_then(serde_json::Value::as_str) == Some(owner_session_id);
        let already_owned = fields
            .get("owner_session_id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|owner| !owner.is_empty());
        if launched_here && !already_owned {
            let _replaced =
                fields.insert("owner_session_id".to_string(), serde_json::Value::String(owner_session_id.to_string()));
            migrated += 1;
        }
    }
    migrated
}

/// Rewrites the ledger with the migrated ownership.
///
/// The caller holds the ledger's inter-process lock, so this read-modify-write
/// cannot race a save from another process.
async fn write_migrated_ledger_state(path: &Path, state: &serde_json::Value) -> anyhow::Result<()> {
    let payload = serde_json::to_string_pretty(state).context("Failed to serialize background subagent state")?;
    diapason_commons::fs::write_file_atomic_with_context(path, &payload, "background subagent owner migration").await
}

/// Restores the background records an owner-scoped controller holds, rolling
/// back a launch whose identity could not be persisted.
pub(super) async fn restore_owned(
    controller: &SubagentController,
    workspace_root: &Path,
) -> anyhow::Result<Vec<BackgroundSubprocessEntry>> {
    restore_with_rollback(controller, workspace_root, controller.restore_background_subagents()).await
}

/// Restores background records that predate session ownership, rolling back a
/// launch whose identity could not be persisted.
pub(super) async fn restore_legacy(
    controller: &SubagentController,
    workspace_root: &Path,
) -> anyhow::Result<Vec<BackgroundSubprocessEntry>> {
    restore_with_rollback(controller, workspace_root, controller.restore_legacy_background_subagents()).await
}

/// Runs one restore pass and rolls back its unrecorded launches on failure.
///
/// The snapshot is taken before the pass so a launch made by it is
/// distinguishable from a launch that was already running.
async fn restore_with_rollback(
    controller: &SubagentController,
    workspace_root: &Path,
    restore: impl Future<Output = anyhow::Result<Vec<BackgroundSubprocessEntry>>>,
) -> anyhow::Result<Vec<BackgroundSubprocessEntry>> {
    let before = launch_snapshot(controller.background_status_entries(TaskOwner::Workspace).await);
    match restore.await {
        Ok(restored) => Ok(restored),
        Err(error) => {
            rollback_unpersisted_launches(controller, workspace_root, &before).await;
            Err(error)
        }
    }
}

/// The exec session each record named before a restore pass began.
fn launch_snapshot(entries: Vec<BackgroundSubprocessEntry>) -> HashMap<String, String> {
    entries.into_iter().map(|entry| (entry.id, entry.exec_session_id)).collect()
}

/// Stops every launch a failed restore made whose exec session the ledger does
/// not record.
///
/// A record whose exec session is unchanged cannot name a process this pass
/// started, a launch the ledger already names is recoverable, and a record the
/// commit did not mark `Running` has no process to stop, so each is left
/// running. Only a changed, running, unrecorded launch is stopped; its stop
/// also marks the record terminal, so a retried attach cannot relaunch the
/// process behind the failure. When the ledger cannot be verified nothing is
/// rolled back: an unverifiable ledger must not authorise destroying a launch
/// that may be durable, while the in-memory controller still tracks the live
/// process.
async fn rollback_unpersisted_launches(
    controller: &SubagentController,
    workspace_root: &Path,
    before: &HashMap<String, String>,
) {
    let after = controller.background_status_entries(TaskOwner::Workspace).await;
    let persisted = persisted_launches(workspace_root).await;
    if matches!(persisted, PersistedLaunches::Unknown) {
        tracing::warn!(
            operation = "background_restore_rollback",
            error_category = "background_ledger_unverifiable",
            "Skipping background launch rollback because the persisted ledger could not be verified"
        );
        return;
    }
    // The cancellation presents the record's own owner, so a session controller
    // may stop only a launch it owns while a workspace controller stops the
    // ownerless rows it restored. Nothing here can reach a foreign session's
    // record.
    let owners = after
        .iter()
        .map(|entry| (entry.id.as_str(), entry.owner_session_id.as_deref()))
        .collect::<HashMap<_, _>>();
    for record_id in unpersisted_launches(before, &after, &persisted) {
        let owner = owners
            .get(record_id.as_str())
            .copied()
            .flatten()
            .map_or(TaskOwner::Workspace, TaskOwner::Session);
        match controller.force_cancel_background(owner, &record_id).await {
            Ok(outcome) if outcome.is_close_pending() => {
                // The record is already terminal and saved, so a retried attach
                // will not relaunch it. The exec session close is unconfirmed,
                // though, so the process may still be live.
                tracing::warn!(
                    record_id = record_id.as_str(),
                    operation = "background_restore_rollback",
                    error_category = "background_launch_close_pending",
                    "Rolled back a background launch whose identity could not be persisted, but its exec session close is unconfirmed"
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    record_id = record_id.as_str(),
                    operation = "background_restore_rollback",
                    error_category = "background_launch_cancel_failed",
                    error_chain_depth = error.chain().count(),
                    "Failed to roll back a background launch whose identity could not be persisted"
                );
            }
        }
    }
}

/// Selects the record ids a failed restore must roll back.
///
/// The snapshot and the post-restore view are keyed by the canonical record id
/// core uses. Only a launch the commit marked `Running` whose exec session is
/// non-empty, differs from the snapshot, and is absent from a verifiable ledger
/// is selected. An unverifiable ledger selects nothing.
fn unpersisted_launches(
    before: &HashMap<String, String>,
    after: &[BackgroundSubprocessEntry],
    persisted: &PersistedLaunches,
) -> Vec<String> {
    if matches!(persisted, PersistedLaunches::Unknown) {
        return Vec::new();
    }
    after
        .iter()
        .filter(|entry| matches!(entry.status, BackgroundSubprocessStatus::Running))
        .filter(|entry| !entry.exec_session_id.is_empty())
        .filter(|entry| before.get(&entry.id) != Some(&entry.exec_session_id))
        .filter(|entry| !persisted.records(&entry.exec_session_id))
        .map(|entry| entry.id.clone())
        .collect()
}

/// What the persisted ledger proves about the launches a restore made.
///
/// An empty set and "could not tell" are different facts: treating a failed
/// read as "records nothing" would authorise stopping a launch that the ledger
/// may still record, so the caller must distinguish them.
#[derive(Debug, PartialEq, Eq)]
enum PersistedLaunches {
    /// No ledger exists, so no launch is recorded.
    Absent,
    /// The ledger was read and parsed; these are its recorded exec sessions.
    Known(HashSet<String>),
    /// The ledger exists but could not be read or parsed.
    Unknown,
}

impl PersistedLaunches {
    /// Whether the ledger verifiably records this exec session as launched.
    ///
    /// [`PersistedLaunches::Unknown`] is rejected before selection, so its
    /// answer here is only a fail-safe default.
    fn records(&self, exec_session_id: &str) -> bool {
        match self {
            Self::Known(exec_session_ids) => exec_session_ids.contains(exec_session_id),
            Self::Absent | Self::Unknown => false,
        }
    }
}

/// Reads which exec sessions the ledger records as launched.
///
/// A missing ledger is [`PersistedLaunches::Absent`]. A read or parse failure
/// is [`PersistedLaunches::Unknown`] rather than an empty set: the ledger may
/// still record a launch this pass cannot see, and degrading that to "records
/// nothing" would authorise destroying a durable process.
async fn persisted_launches(workspace_root: &Path) -> PersistedLaunches {
    let path = ledger_path(workspace_root);
    let raw = match fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return PersistedLaunches::Absent,
        Err(_) => return PersistedLaunches::Unknown,
    };
    let Ok(state) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return PersistedLaunches::Unknown;
    };
    let exec_session_ids = state
        .get("records")
        .and_then(serde_json::Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(|record| record.get("exec_session_id").and_then(serde_json::Value::as_str))
                .filter(|exec_session_id| !exec_session_id.is_empty())
                .map(str::to_owned)
                .collect::<HashSet<String>>()
        })
        .unwrap_or_default();
    PersistedLaunches::Known(exec_session_ids)
}
