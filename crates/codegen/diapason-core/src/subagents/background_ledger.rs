//! Persisted background-ledger records and the durable evidence a legacy owner
//! migration leaves behind.
//!
//! The ledger is shared by every controller in a workspace, so its record type
//! and the rules that reconcile two copies live together here rather than in
//! the module that defines observer and tool contracts. Serde names are part of
//! the on-disk schema and must not change.
mod migration;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

use super::types::{BackgroundRecord, BackgroundSubprocessStatus};
use crate::utils::file_utils::write_file_atomic_with_context;

pub(crate) use self::migration::MigratedBackgroundLaunch;
use self::migration::retained_migrated_launches;

/// One background record as it is stored in the shared ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedBackgroundRecord {
    pub(crate) id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) owner_session_id: Option<String>,
    pub(crate) agent_name: String,
    pub(crate) display_label: String,
    pub(crate) description: String,
    pub(crate) source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour")]
    pub(crate) colour: Option<String>,
    pub(crate) session_id: String,
    /// Session parent that owns this long-lived task. Older state files
    /// predate owner tracking and deserialize to an empty owner.
    #[serde(default)]
    pub(crate) parent_session_id: String,
    /// Read by the persistence join, which groups a record's copies by launch.
    pub(crate) exec_session_id: String,
    pub(crate) desired_enabled: bool,
    pub(crate) status: BackgroundSubprocessStatus,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) pid: Option<u32>,
    pub(crate) prompt: String,
    pub(crate) summary: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) transcript_path: Option<PathBuf>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
    pub(crate) restart_attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedBackgroundState {
    #[serde(default)]
    pub(crate) records: Vec<PersistedBackgroundRecord>,
}

/// The ledger file, including the migration evidence kept beside its records.
///
/// The evidence is a top-level key rather than a record so it survives the
/// replacement that removes the launch it describes. The owner migration
/// rewrites the file as generic JSON, so it preserves the key unchanged.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BackgroundLedgerFile {
    #[serde(default)]
    records: Vec<PersistedBackgroundRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    migrated_launches: Vec<MigratedBackgroundLaunch>,
}

/// The records and migration evidence read from one workspace ledger.
pub(super) struct LoadedBackgroundLedger {
    pub(super) records: Vec<PersistedBackgroundRecord>,
    pub(super) migrated_launches: Vec<MigratedBackgroundLaunch>,
}

/// Reports whether two copies name the same launch through its exec session.
///
/// A named exec session is the launch identity: two copies of it name the same
/// process, whatever row they were stored under. An empty session identifies
/// nothing, so a shared blank value never establishes a launch on its own.
fn same_named_launch(exec_session_id: &str, other_exec_session_id: &str) -> bool {
    !exec_session_id.is_empty() && exec_session_id == other_exec_session_id
}

/// Reports whether two legacy copies name the same child session.
///
/// A legacy row predates the exec-session field, but it still records the child
/// session it was launched for, and every launch mints a fresh one. Two legacy
/// rows therefore describe one launch only when they name the same non-empty
/// child session; a shared blank value is not an identity.
fn same_legacy_launch(session_id: &str, other_session_id: &str) -> bool {
    !session_id.is_empty() && session_id == other_session_id
}

/// Reports whether two copies describe the same launch, named or legacy.
///
/// The exec session is the launch identity when one is named. When neither copy
/// names one, the child session each carries is the identity left. This is the
/// rule a cancellation is compared under and the rule the save merge groups
/// copies under, so the two cannot disagree about whether a stop governs a
/// refresh.
fn same_launch_identity(
    exec_session_id: &str,
    session_id: &str,
    other_exec_session_id: &str,
    other_session_id: &str,
) -> bool {
    same_named_launch(exec_session_id, other_exec_session_id)
        || (exec_session_id.is_empty()
            && other_exec_session_id.is_empty()
            && same_legacy_launch(session_id, other_session_id))
}

/// Reports whether a persisted stop may be adopted onto a record under `id`.
///
/// A named exec session is the identity, as above. A legacy row names no
/// session, so the identity falls back to the canonical record id together with
/// the child session the row names: the ledger keeps one row per id, but that
/// row may describe a different legacy launch than the cached record under the
/// id. A legacy row is therefore adopted only when its id and its child session
/// both agree with the cached record; a blank child session proves nothing. A
/// legacy row never matches a record naming an exec session.
fn adoptable_launch_identity(
    id: &str,
    exec_session_id: &str,
    session_id: &str,
    other_id: &str,
    other_exec_session_id: &str,
    other_session_id: &str,
) -> bool {
    same_named_launch(exec_session_id, other_exec_session_id)
        || (exec_session_id.is_empty()
            && other_exec_session_id.is_empty()
            && !id.is_empty()
            && id == other_id
            && same_legacy_launch(session_id, other_session_id))
}

fn ledger_path(workspace_root: &Path) -> PathBuf {
    workspace_root
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.json")
}

/// Reads the ledger together with the migration evidence it retains.
pub(super) async fn load_background_ledger(workspace_root: &Path) -> Result<LoadedBackgroundLedger> {
    let path = ledger_path(workspace_root);
    let raw = match fs::read_to_string(&path).await {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LoadedBackgroundLedger { records: Vec::new(), migrated_launches: Vec::new() });
        }
        Err(error) => return Err(error).with_context(|| format!("Failed to read {}", path.display())),
    };
    let ledger: BackgroundLedgerFile =
        serde_json::from_str(&raw).with_context(|| format!("Failed to parse {}", path.display()))?;
    Ok(LoadedBackgroundLedger {
        records: ledger.records,
        migrated_launches: ledger.migrated_launches,
    })
}

/// Writes the ledger, retaining the bounded migration evidence beside its records.
pub(super) async fn persist_background_ledger(
    workspace_root: &Path,
    records: Vec<PersistedBackgroundRecord>,
    migrated_launches: &[MigratedBackgroundLaunch],
) -> Result<()> {
    let path = ledger_path(workspace_root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .await
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let migrated_launches = retained_migrated_launches(migrated_launches.iter().cloned());
    let payload = serde_json::to_string_pretty(&BackgroundLedgerFile { records, migrated_launches })?;
    write_file_atomic_with_context(&path, &payload, "background subagent state").await
}

impl PersistedBackgroundRecord {
    /// Rewrites the persisted id into its canonical owner-scoped form.
    ///
    /// State written before ids were owner-scoped stored the
    /// workspace-global id for records that carry an owner. Canonicalizing on
    /// load keeps a relaunch looking at the same record instead of creating a
    /// second one, and keeps every id derivation identical to the launch path.
    pub(crate) fn canonicalize_id(&mut self) {
        self.id = super::background::background_record_id_for_owner(
            self.agent_name.as_str(),
            self.owner_session_id.as_deref(),
        );
    }

    /// Returns the workspace-global id this record had before ownership.
    ///
    /// Migration evidence names a launch by the ownerless id its stale copy
    /// still carries, so a records-side launch is translated to that form
    /// before the two are compared.
    pub(crate) fn ownerless_base_id(&self) -> String {
        super::background::background_record_id_for_owner(self.agent_name.as_str(), None)
    }

    /// The launch instant this copy records, as the ordering evidence for restarts.
    ///
    /// A refresh advances `updated_at` without changing intent or exec session,
    /// so the write time cannot prove a launch is newer. `started_at` is stamped
    /// once when the process starts, and it is the only instant that is evidence
    /// of a launch. A copy that never started is a claim: it records no launch at
    /// all, so it reports the earliest representable instant instead of its
    /// mutable write time. Ordering it that way is what stops a claim whose
    /// `updated_at` some pass advanced from outranking a launch that really
    /// began, while two claims still compare equal and leave their write times to
    /// the merge's freshness rule.
    pub(crate) fn launch_moment(&self) -> DateTime<Utc> {
        self.started_at.unwrap_or(DateTime::<Utc>::MIN_UTC)
    }

    /// Whether this record was written at or after `other`.
    ///
    /// A merge of two copies of the same ledger entry keeps the newer one, so
    /// a local refresh or cancellation is not overwritten by the stale copy
    /// the controller loaded.
    pub(crate) fn is_at_least_as_new_as(&self, other: &Self) -> bool {
        self.updated_at >= other.updated_at
    }

    /// Reports whether this record is a copy of the same launch as `other`.
    ///
    /// The exec session identifies the process a record describes, so two
    /// records naming the same non-empty session are copies of one launch. A
    /// legacy row names none, and two of them may describe entirely different
    /// child sessions under one canonical id; the child session each names is
    /// then the identity left, and a shared blank value never stands in for
    /// one. Nothing here falls back to the record id: a precedence decision
    /// between two rows must never be made on intent alone, so only
    /// [`Self::adopt_cancellation_into`] may apply that fallback.
    pub(crate) fn shares_launch_identity_with(&self, other: &Self) -> bool {
        same_launch_identity(&self.exec_session_id, &self.session_id, &other.exec_session_id, &other.session_id)
    }

    /// Whether this record's stop outranks `snapshot` regardless of timestamps.
    ///
    /// A stop is one-way until a restart replaces the launch: an explicit
    /// restart writes a fresh exec session, whereas a controller that merely
    /// refreshed its stale copy keeps the session this record stopped and
    /// advances `updated_at` without changing intent. Timestamp order alone
    /// therefore cannot decide this case, or a refresh would resurrect a
    /// cancelled task.
    ///
    /// The rule only applies to copies of one identified launch. Two legacy
    /// rows that both name no session are not shown to describe the same
    /// process, so neither may claim precedence over the other on intent alone;
    /// adoption is the only transition allowed to fall back to the record id.
    pub(crate) fn cancellation_outranks(&self, snapshot: &Self) -> bool {
        !self.desired_enabled && snapshot.desired_enabled && self.shares_launch_identity_with(snapshot)
    }

    /// Applies this record's stop to a cached copy of the same launch.
    ///
    /// Returns `false` when this record is not a cancellation of that launch:
    /// it is still desired, the cached copy is already stopped, or the two
    /// name different exec sessions, which means a restart has replaced the
    /// session this record stopped. Adoption clears the cached desire so
    /// refresh cannot relaunch it.
    ///
    /// The adopted status is terminal only when there is no exec session left
    /// to close. A live session means the process may still be running, so the
    /// copy keeps an active status with a stopping summary: the caller must
    /// attempt the close and let it settle `Stopped` on success. Writing the
    /// persisted terminal status here would hide a running process from the
    /// liveness projection, because that projection skips non-active records.
    ///
    /// Only a copy of the same launch may be adopted. A named exec session is
    /// that identity. Legacy rows name none, so the identity falls back to the
    /// canonical record id both sides are keyed by *and* to the child session
    /// each names: the ledger keeps one row per canonical id, but that row may
    /// describe a different legacy launch than the cached record. A blank
    /// child session proves nothing. A legacy row is still never a copy of a
    /// launch that names a session, which is what keeps a stub from stopping a
    /// live process.
    pub(crate) fn adopt_cancellation_into(&self, record: &mut BackgroundRecord) -> bool {
        if self.desired_enabled || !record.desired_enabled {
            return false;
        }
        // `BackgroundRecord` is not `PersistedBackgroundRecord`, so the shared
        // rule is applied to the ids it carries.
        if !adoptable_launch_identity(
            &self.id,
            &self.exec_session_id,
            &self.session_id,
            &record.id,
            &record.exec_session_id,
            &record.session_id,
        ) {
            return false;
        }
        record.desired_enabled = false;
        if record.exec_session_id.is_empty() {
            // Nothing can be running under the record, so the persisted
            // outcome is already final.
            if record.status.is_active() {
                record.status = self.status;
                record.error = self.error.clone();
                record.ended_at = self.ended_at.or(record.ended_at);
            }
        } else if record.status.is_active() {
            // The close has not been attempted yet, so the process may still be
            // live. Keep the record active and carry the stop intent; the
            // close settles the terminal outcome only after it succeeds.
            record.status = BackgroundSubprocessStatus::Running;
            record.summary = Some("Background subagent is stopping".to_string());
            record.ended_at = None;
        }
        record.updated_at = record.updated_at.max(self.updated_at);
        true
    }
}

impl BackgroundRecord {
    pub(crate) fn into_persisted(self) -> PersistedBackgroundRecord {
        PersistedBackgroundRecord {
            id: self.id,
            owner_session_id: self.owner_session_id,
            agent_name: self.agent_name,
            display_label: self.display_label,
            description: self.description,
            source: self.source,
            colour: self.colour,
            session_id: self.session_id,
            parent_session_id: self.parent_session_id,
            exec_session_id: self.exec_session_id,
            desired_enabled: self.desired_enabled,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            pid: self.pid,
            prompt: self.prompt,
            summary: self.summary,
            error: self.error,
            archive_path: self.archive_path,
            transcript_path: self.transcript_path,
            max_turns: self.max_turns,
            model_override: self.model_override,
            reasoning_override: self.reasoning_override,
            restart_attempts: self.restart_attempts,
        }
    }

    pub(crate) fn from_persisted(record: PersistedBackgroundRecord) -> Self {
        Self {
            id: record.id,
            owner_session_id: record.owner_session_id,
            agent_name: record.agent_name,
            display_label: record.display_label,
            description: record.description,
            source: record.source,
            colour: record.colour,
            session_id: record.session_id,
            parent_session_id: record.parent_session_id,
            exec_session_id: record.exec_session_id,
            desired_enabled: record.desired_enabled,
            status: record.status,
            created_at: record.created_at,
            updated_at: record.updated_at,
            started_at: record.started_at,
            ended_at: record.ended_at,
            pid: record.pid,
            prompt: record.prompt,
            summary: record.summary,
            error: record.error,
            archive_path: record.archive_path,
            transcript_path: record.transcript_path,
            max_turns: record.max_turns,
            model_override: record.model_override,
            reasoning_override: record.reasoning_override,
            restart_attempts: record.restart_attempts,
        }
    }
}

#[cfg(test)]
mod launch_identity_tests;

#[cfg(test)]
mod tests {
    //! Verifies the persisted record keeps its documented colour aliases and
    //! reports only recorded launches as ordering evidence.

    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    /// A persisted record carrying the given start and write instants.
    fn persisted_launch(started_at: Option<DateTime<Utc>>, updated_at: DateTime<Utc>) -> PersistedBackgroundRecord {
        PersistedBackgroundRecord {
            id: "background-worker".to_string(),
            agent_name: "worker".to_string(),
            display_label: "Worker".to_string(),
            description: "A persistence fixture".to_string(),
            source: "project".to_string(),
            colour: None,
            owner_session_id: None,
            session_id: "session-1".to_string(),
            parent_session_id: "parent-session".to_string(),
            exec_session_id: "exec-1".to_string(),
            desired_enabled: true,
            status: BackgroundSubprocessStatus::Running,
            created_at: DateTime::<Utc>::UNIX_EPOCH,
            updated_at,
            started_at,
            ended_at: None,
            pid: None,
            prompt: "resume".to_string(),
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
        }
    }

    #[test]
    fn persisted_background_record_accepts_colour_and_serializes_the_fixed_key() {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        let mut record = persisted_launch(Some(now), now);
        record.colour = Some("orchid".to_string());

        let mut canonical = serde_json::to_value(&record).expect("the persisted record should serialize");
        assert_eq!(canonical.get("color"), Some(&json!("orchid")), "persisted state should use the fixed wire key");
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized persisted record should be an object")
            .remove("color")
            .expect("the fixed wire key should be present");
        let previous_british_alias = canonical
            .as_object_mut()
            .expect("a serialized persisted record should remain an object")
            .insert("colour".to_string(), canonical_value);
        assert!(previous_british_alias.is_none(), "the British alias should be absent before deserialization");

        let british: PersistedBackgroundRecord =
            serde_json::from_value(canonical).expect("the British persisted-record alias should deserialize");
        assert_eq!(british.colour.as_deref(), Some("orchid"), "the British alias should preserve persisted colour");
    }

    /// A claim records no launch, so a started launch always outranks it.
    ///
    /// The claim's `updated_at` is a write time that any later pass can
    /// advance. Ordering a started replacement against it let a claim that never
    /// ran keep the ledger row, so the process that really began was dropped
    /// from the ledger. The recorded start must win whatever the claim's write
    /// time says.
    #[test]
    fn a_started_launch_outranks_a_refreshed_unstarted_claim() {
        let claim_write_time = DateTime::<Utc>::UNIX_EPOCH + chrono::Duration::days(1);
        let started_at = DateTime::<Utc>::UNIX_EPOCH;
        let claim = persisted_launch(None, claim_write_time);
        let started = persisted_launch(Some(started_at), started_at);

        assert!(
            claim.launch_moment() < claim.updated_at,
            "an unstarted claim must not report its write time as a launch moment"
        );
        assert!(
            started.launch_moment() >= claim.launch_moment(),
            "a launch that recorded its start must outrank an unstarted claim whose write time is later"
        );
    }

    /// The extended ledger path stays the one the rest of the module reads.
    #[test]
    fn the_ledger_path_matches_the_shared_helper() {
        let temp = tempfile::TempDir::new().expect("temporary workspace");

        assert_eq!(
            ledger_path(temp.path()),
            crate::subagents::background::background_state_path_for_tests(temp.path()),
            "the ledger and its migration evidence must live in the same file"
        );
    }
}
