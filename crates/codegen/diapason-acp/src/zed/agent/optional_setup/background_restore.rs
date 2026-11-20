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
    let raw = match fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", path.display()));
        }
    };
    let mut state: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
    let Some(records) = state.get_mut("records").and_then(serde_json::Value::as_array_mut) else {
        return Ok(0);
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
    if migrated == 0 {
        return Ok(0);
    }
    let payload = serde_json::to_string_pretty(&state).context("Failed to serialize background subagent state")?;
    diapason_commons::fs::write_file_atomic_with_context(&path, &payload, "background subagent owner migration")
        .await?;
    Ok(migrated)
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
    let before = launch_snapshot(controller.background_status_entries().await);
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
    let after = controller.background_status_entries().await;
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
        if let Err(error) = controller.force_cancel_background(owner, &record_id).await {
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

/// Regressions for owner migration and launch rollback.
#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use serde_json::json;

    async fn write_ledger(workspace: &Path, records: serde_json::Value) {
        let path = ledger_path(workspace);
        fs::create_dir_all(path.parent().expect("ledger parent"))
            .await
            .expect("state directory");
        fs::write(&path, serde_json::to_string_pretty(&json!({ "records": records })).expect("serialize ledger"))
            .await
            .expect("write ledger");
    }

    async fn read_ledger(workspace: &Path) -> serde_json::Value {
        let raw = fs::read_to_string(ledger_path(workspace)).await.expect("read ledger");
        serde_json::from_str(&raw).expect("parse ledger")
    }

    fn entry(id: &str, exec_session_id: &str) -> BackgroundSubprocessEntry {
        entry_with_status(id, exec_session_id, "running")
    }

    /// Builds a status fixture, so a failed spawn that keeps its newly assigned
    /// exec session can be represented as the `error` record core settles.
    fn entry_with_status(id: &str, exec_session_id: &str, status: &str) -> BackgroundSubprocessEntry {
        serde_json::from_value(json!({
            "id": id,
            "session_id": format!("session-{id}"),
            "exec_session_id": exec_session_id,
            "agent_name": "watcher",
            "display_label": id,
            "description": id,
            "source": "test",
            "status": status,
            "desired_enabled": true,
            "created_at": "2026-08-14T12:00:00Z",
            "updated_at": "2026-08-14T12:00:00Z",
        }))
        .expect("background entry fixture")
    }

    /// A row this session launched must become owned by it, so the workspace
    /// restore skips it and the session controller adopts it. A row another
    /// session launched, and one that already has an owner, must be untouched.
    #[tokio::test]
    async fn a_legacy_row_is_adopted_by_the_session_that_launched_it() {
        let temp = TempDir::new().expect("temporary workspace");
        write_ledger(
            temp.path(),
            json!([
                {
                    "id": "background-watcher",
                    "agent_name": "watcher",
                    "parent_session_id": "session-a",
                    "exec_session_id": "exec-a",
                    "desired_enabled": true,
                },
                {
                    "id": "background-watcher@other",
                    "agent_name": "watcher",
                    "owner_session_id": "other",
                    "parent_session_id": "session-a",
                    "exec_session_id": "exec-b",
                    "desired_enabled": true,
                },
                {
                    "id": "background-watcher-foreign",
                    "agent_name": "watcher",
                    "parent_session_id": "session-b",
                    "exec_session_id": "exec-c",
                    "desired_enabled": true,
                },
            ]),
        )
        .await;

        let migrated = migrate_legacy_owner_rows(temp.path(), "session-a").await.expect("migration");

        assert_eq!(migrated, 1, "only the unowned row launched by this session is migrated");
        let ledger = read_ledger(temp.path()).await;
        let records = ledger["records"].as_array().expect("records");
        assert_eq!(records[0]["owner_session_id"], "session-a");
        assert_eq!(records[0]["parent_session_id"], "session-a", "ownership must not rewrite the launcher");
        assert_eq!(records[0]["exec_session_id"], "exec-a", "the launch identity must be preserved");
        assert_eq!(records[0]["desired_enabled"], true);
        assert_eq!(records[1]["owner_session_id"], "other", "an owned row must never change hands");
        assert_eq!(records[2].get("owner_session_id"), None, "another session's row stays ownerless");
    }

    /// A retried attach must not reinterpret an already migrated row.
    #[tokio::test]
    async fn migration_is_idempotent() {
        let temp = TempDir::new().expect("temporary workspace");
        write_ledger(
            temp.path(),
            json!([{
                "id": "background-watcher",
                "agent_name": "watcher",
                "parent_session_id": "session-a",
                "exec_session_id": "exec-a",
                "desired_enabled": true,
            }]),
        )
        .await;

        assert_eq!(
            migrate_legacy_owner_rows(temp.path(), "session-a")
                .await
                .expect("first migration"),
            1
        );
        assert_eq!(
            migrate_legacy_owner_rows(temp.path(), "session-a")
                .await
                .expect("second migration"),
            0
        );
    }

    /// A workspace with no ledger has nothing to migrate, and the migration
    /// must not create one.
    #[tokio::test]
    async fn a_missing_ledger_is_a_no_op() {
        let temp = TempDir::new().expect("temporary workspace");
        assert_eq!(migrate_legacy_owner_rows(temp.path(), "session-a").await.expect("migration"), 0);
        assert!(!ledger_path(temp.path()).exists(), "a no-op migration must not create the ledger");
    }

    /// Only a changed, running, unrecorded launch may be rolled back. An
    /// unchanged record predates the pass, a recorded launch is recoverable, a
    /// record whose exec session was cleared names no process, and a record a
    /// failed spawn settled `Error` never had a process to stop.
    #[test]
    fn rollback_selects_only_launches_the_ledger_does_not_record() {
        let before = HashMap::from([
            ("background-a".to_string(), "exec-old-a".to_string()),
            ("background-b".to_string(), "exec-old-b".to_string()),
            ("background-c".to_string(), "exec-old-c".to_string()),
            ("background-d".to_string(), "exec-old-d".to_string()),
            ("background-e".to_string(), "exec-old-e".to_string()),
        ]);
        let after = vec![
            entry("background-a", "exec-new-a"),
            entry("background-b", "exec-old-b"),
            entry("background-c", ""),
            entry("background-d", "exec-new-d"),
            entry_with_status("background-e", "exec-new-e", "error"),
        ];
        let persisted = PersistedLaunches::Known(HashSet::from(["exec-new-d".to_string()]));

        assert_eq!(
            unpersisted_launches(&before, &after, &persisted),
            vec!["background-a".to_string()],
            "only the running launch that changed and was not persisted may be stopped"
        );
    }

    /// An unverifiable ledger must not authorise stopping anything: a launch it
    /// might still record could be durable, and the in-memory controller still
    /// tracks the live process.
    #[test]
    fn rollback_selects_nothing_when_the_ledger_is_unverifiable() {
        let before = HashMap::from([("background-a".to_string(), "exec-old-a".to_string())]);
        let after = vec![entry("background-a", "exec-new-a")];

        assert!(
            unpersisted_launches(&before, &after, &PersistedLaunches::Unknown).is_empty(),
            "an unverifiable ledger must leave a possibly durable launch running"
        );
    }

    /// A missing ledger still means no launch is recorded, so the launch this
    /// pass made is rolled back.
    #[test]
    fn rollback_selects_a_launch_when_no_ledger_exists() {
        let before = HashMap::from([("background-a".to_string(), "exec-old-a".to_string())]);
        let after = vec![entry("background-a", "exec-new-a")];

        assert_eq!(
            unpersisted_launches(&before, &after, &PersistedLaunches::Absent),
            vec!["background-a".to_string()],
            "an absent ledger records no launch"
        );
    }

    /// The ledger is the only record of which launches survived a failed save.
    #[tokio::test]
    async fn persisted_exec_sessions_are_read_from_the_ledger() {
        let temp = TempDir::new().expect("temporary workspace");
        write_ledger(
            temp.path(),
            json!([
                { "id": "background-a", "exec_session_id": "exec-a" },
                { "id": "background-b", "exec_session_id": "" },
                { "id": "background-c" },
            ]),
        )
        .await;

        assert_eq!(
            persisted_launches(temp.path()).await,
            PersistedLaunches::Known(HashSet::from(["exec-a".to_string()])),
            "empty and absent exec sessions are not persisted launches"
        );
    }

    /// A workspace with no ledger records no launch.
    #[tokio::test]
    async fn a_missing_ledger_records_no_persisted_launch() {
        let temp = TempDir::new().expect("temporary workspace");
        assert_eq!(persisted_launches(temp.path()).await, PersistedLaunches::Absent);
    }

    /// A ledger that cannot be read is unverifiable, not empty: the launch may
    /// still be durable, so rollback must not treat it as unrecorded.
    #[tokio::test]
    async fn a_faulting_ledger_makes_launches_unverifiable() {
        let temp = TempDir::new().expect("temporary workspace");
        let path = ledger_path(temp.path());
        fs::create_dir_all(path.parent().expect("ledger parent"))
            .await
            .expect("state directory");
        fs::create_dir(&path).await.expect("occupy the ledger path with a directory");

        assert_eq!(
            persisted_launches(temp.path()).await,
            PersistedLaunches::Unknown,
            "an unreadable ledger must not be degraded to an empty set"
        );
    }

    /// A ledger whose contents cannot be parsed is likewise unverifiable.
    #[tokio::test]
    async fn a_malformed_ledger_makes_launches_unverifiable() {
        let temp = TempDir::new().expect("temporary workspace");
        let path = ledger_path(temp.path());
        fs::create_dir_all(path.parent().expect("ledger parent"))
            .await
            .expect("state directory");
        fs::write(&path, b"{ not json").await.expect("write malformed ledger");

        assert_eq!(
            persisted_launches(temp.path()).await,
            PersistedLaunches::Unknown,
            "a parse failure must not be degraded to an empty set"
        );
    }
}
