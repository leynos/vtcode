//! Retention and garbage-collection for the unified session store.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use walkdir::WalkDir;

use crate::error::SessionStoreError;
use crate::query::SessionSummary;
use crate::sessions_root;

#[derive(Debug)]
struct RetentionCandidate {
    path: std::path::PathBuf,
    summary: SessionSummary,
}

/// Retention policy applied to the set of per-session stores.
#[derive(Debug, Clone, Copy)]
pub struct RetentionPolicy {
    /// Maximum number of sessions to keep (oldest evicted first).
    pub max_sessions: usize,
    /// Maximum age of a session in days before eviction.
    pub max_age_days: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self { max_sessions: 50, max_age_days: 30 }
    }
}

/// Apply the retention policy, removing the oldest / stale sessions.
///
/// Returns the number of sessions removed. This bounds the otherwise
/// unbounded growth of `.vtcode/sessions/` so overhead does not accumulate
/// on disk across a long-lived agent.
pub fn apply_retention(workspace: &Path, policy: RetentionPolicy) -> Result<usize, SessionStoreError> {
    apply_retention_preserving(workspace, policy, None)
}

/// Apply retention while preserving one session directory, even when its
/// existing manifest still says `completed` (for example, a resumed session).
///
/// The preserved path is resolved with the same session-id sanitization as the
/// canonical store and is compared against the direct child discovered on
/// disk. It is never taken from a manifest.
pub fn apply_retention_preserving(
    workspace: &Path,
    policy: RetentionPolicy,
    preserve_session_id: Option<&str>,
) -> Result<usize, SessionStoreError> {
    let root = sessions_root(workspace);
    let root_metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(SessionStoreError::io(root.clone(), error)),
    };
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Ok(0);
    }
    let preserve_path = preserve_session_id.map(|session_id| crate::session_dir(workspace, session_id));
    let mut sessions = retention_candidates(&root, preserve_path.as_deref())?;
    let mut removed = 0usize;

    // Phase 1: evict oldest sessions beyond the count cap.
    if sessions.len() > policy.max_sessions {
        sessions.sort_by(|a, b| a.summary.updated_at.cmp(&b.summary.updated_at));
        let to_remove = sessions.len() - policy.max_sessions;
        for s in sessions.iter().take(to_remove) {
            remove_session(&root, &s.path)?;
            removed += 1;
        }
        // Drop evicted entries so phase 2 doesn't double-remove.
        sessions.drain(..to_remove);
    }

    // Phase 2: evict sessions older than max_age_days (regardless of count).
    let cutoff = age_cutoff(policy.max_age_days);
    for s in &sessions {
        if older_than(s.summary.updated_at.as_str(), cutoff) {
            remove_session(&root, &s.path)?;
            removed += 1;
        }
    }

    Ok(removed)
}

/// Enumerate session stores from their filesystem entries, never from the
/// session ID contained in a manifest. This keeps retention confined to
/// validated direct children of the sessions root.
fn retention_candidates(
    root: &Path,
    preserve_path: Option<&Path>,
) -> Result<Vec<RetentionCandidate>, SessionStoreError> {
    let entries = std::fs::read_dir(root).map_err(|e| SessionStoreError::io(root.to_path_buf(), e))?;
    let mut candidates = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| SessionStoreError::io(root.to_path_buf(), e))?;
        let file_type = entry.file_type().map_err(|e| SessionStoreError::io(entry.path(), e))?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = entry.path();
        if preserve_path.is_some_and(|preserve_path| preserve_path == path) {
            continue;
        }
        let manifest_path = path.join("manifest.json");
        let Ok(bytes) = std::fs::read(&manifest_path) else {
            continue;
        };
        let Ok(summary) = serde_json::from_slice::<SessionSummary>(&bytes) else {
            continue;
        };
        if summary.status == "active" {
            continue;
        }
        candidates.push(RetentionCandidate { path, summary });
    }
    Ok(candidates)
}

/// Remove the legacy `history/` and `logs/` directories after they have been
/// imported into the unified store by [`crate::migrate_legacy`].
///
/// Returns the number of bytes freed. The legacy `checkpoints/` directory is
/// intentionally left in place until `/revert` is rewired to the unified
/// store; callers should confirm revert behaviour before deleting it manually.
pub fn gc_legacy(workspace: &Path) -> Result<u64, SessionStoreError> {
    let vt = workspace.join(".vtcode");
    let mut freed = 0u64;
    for name in ["history", "logs"] {
        let dir = vt.join(name);
        if dir.exists() {
            freed += dir_size(&dir);
            std::fs::remove_dir_all(&dir).map_err(|e| SessionStoreError::io(dir.clone(), e))?;
        }
    }
    Ok(freed)
}

fn remove_session(root: &Path, dir: &Path) -> Result<(), SessionStoreError> {
    // Retention is allowed to remove only one validated child of the sessions
    // root. Never trust a manifest-controlled identifier or follow a symlink.
    if dir.parent() != Some(root) {
        return Ok(());
    }
    let metadata = match std::fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(SessionStoreError::io(dir.to_path_buf(), error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(());
    }
    std::fs::remove_dir_all(dir).map_err(|e| SessionStoreError::io(dir.to_path_buf(), e))?;
    Ok(())
}

fn dir_size(dir: &Path) -> u64 {
    WalkDir::new(dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

fn age_cutoff(max_age_days: u64) -> SystemTime {
    let seconds = max_age_days.saturating_mul(24 * 3600);
    SystemTime::now() - Duration::from_secs(seconds)
}

fn older_than(rfc3339: &str, cutoff: SystemTime) -> bool {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(rfc3339) else {
        return false;
    };
    let cutoff_secs = cutoff
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(i64::MAX);
    dt.timestamp() < cutoff_secs
}
