//! Serializes and restores durable session history, metadata, and transcript summaries.
use crate::config::{HistoryPersistence, VTCodeConfig};

mod archive;
mod listing;
mod message;
mod metadata;
mod retention;
mod snapshot_compaction;
mod transcript;

use anyhow::{Context, Result};
#[cfg(test)]
use archive::generate_unique_archive_path;
pub use archive::{SessionArchive, generate_session_archive_identifier, reserve_session_archive_identifier};
pub(super) use archive::{is_session_file, session_archive_path_for_identifier, validate_session_identifier};
#[cfg(test)]
use listing::truncate_preview;
pub use listing::{
    SearchResult, find_session_by_id_or_name, find_session_by_identifier, list_recent_sessions, search_sessions,
    session_listing_matches_workspace, session_workspace_path,
};
pub use message::{
    SessionListing, SessionMessage, SessionProgress, SessionProgressArgs, SessionProgressPersistenceStatus,
    SessionSnapshot, VerifiedSessionArchiveIdentifier,
};
pub use metadata::{
    SessionArchiveExtensions, SessionArchiveMetadata, SessionContinuationExhaustionReason, SessionContinuationMetadata,
    SessionContinuationRecommendedAction, SessionForkMode,
};
pub(super) use retention::apply_session_retention_best_effort;
#[cfg(test)]
use retention::{BYTES_PER_MB, SessionRetentionLimits, apply_session_retention_with_limits};
pub(super) use snapshot_compaction::prepare_snapshot_for_write;
#[cfg(test)]
use snapshot_compaction::{
    bound_session_archive_extensions, compact_snapshot_to_max_bytes, serialized_json_size_at_most,
};
use std::env;
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
pub(super) use transcript::{clean_transcript_lines, progress_transcript_from_recent_messages};
use vtcode_commons::VtCodePaths;

pub const SESSION_DIR_ENV: &str = "VT_SESSION_DIR";
pub const SESSION_MAX_FILES_ENV: &str = "VT_SESSION_MAX_FILES";
pub const SESSION_MAX_AGE_DAYS_ENV: &str = "VT_SESSION_MAX_AGE_DAYS";
pub const SESSION_MAX_SIZE_MB_ENV: &str = "VT_SESSION_MAX_SIZE_MB";
/// Maximum number of namespaced extension values retained in a session archive.
pub const MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES: usize = 64;
/// Maximum serialized JSON size of extension values retained in a session archive.
pub const MAX_SESSION_ARCHIVE_EXTENSION_BYTES: usize = 16 * 1024;
/// Namespace used to preserve unrecognized fields from older archive formats.
pub const SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE: &str = "dev.vtcode.archive.legacy_fields";

#[derive(Debug, Clone, Copy)]
struct SessionHistorySettings {
    persistence: HistoryPersistence,
    max_bytes: Option<usize>,
}

impl Default for SessionHistorySettings {
    fn default() -> Self {
        Self {
            persistence: HistoryPersistence::File,
            max_bytes: None,
        }
    }
}

static SESSION_HISTORY_SETTINGS: OnceLock<Mutex<SessionHistorySettings>> = OnceLock::new();

fn session_history_settings() -> SessionHistorySettings {
    SESSION_HISTORY_SETTINGS
        .get()
        .map(|settings| *settings.lock().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or_default()
}

pub fn apply_session_history_config_from_vtcode(config: &VTCodeConfig) {
    let settings = SessionHistorySettings {
        persistence: config.history.persistence,
        max_bytes: config.history.max_bytes,
    };
    let cell = SESSION_HISTORY_SETTINGS.get_or_init(|| Mutex::new(SessionHistorySettings::default()));
    if let Ok(mut guard) = cell.lock() {
        *guard = settings;
    }
}

pub fn history_persistence_enabled() -> bool {
    matches!(session_history_settings().persistence, HistoryPersistence::File)
}

#[cfg(test)]
mod test_env_overrides {
    use hashbrown::HashMap;
    use std::ffi::OsString;
    use std::sync::{LazyLock, Mutex};

    static OVERRIDES: LazyLock<Mutex<HashMap<String, Option<OsString>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(super) fn get(key: &str) -> Option<Option<OsString>> {
        OVERRIDES.lock().ok().and_then(|map| map.get(key).cloned())
    }

    pub(super) fn set(key: &str, value: Option<OsString>) {
        if let Ok(mut map) = OVERRIDES.lock() {
            map.insert(key.to_owned(), value);
        }
    }

    pub(super) fn clear(key: &str) {
        if let Ok(mut map) = OVERRIDES.lock() {
            map.remove(key);
        }
    }
}

fn read_env_var_os(key: &str) -> Option<std::ffi::OsString> {
    #[cfg(test)]
    if let Some(override_value) = test_env_overrides::get(key) {
        return override_value;
    }

    env::var_os(key)
}

fn read_env_var(key: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(override_value) = test_env_overrides::get(key) {
        return override_value.map(|value| value.to_string_lossy().to_string());
    }

    env::var(key).ok()
}

#[cfg(test)]
fn set_test_env_override_path(key: &str, value: &Path) {
    test_env_overrides::set(key, Some(value.as_os_str().to_os_string()));
}

#[cfg(test)]
fn clear_test_env_override(key: &str) {
    test_env_overrides::clear(key);
}

#[cfg(test)]
pub(crate) fn override_sessions_dir_for_tests(path: &Path) {
    set_test_env_override_path(SESSION_DIR_ENV, path);
}

#[cfg(test)]
pub(crate) fn clear_sessions_dir_override_for_tests() {
    clear_test_env_override(SESSION_DIR_ENV);
}

async fn resolve_sessions_dir() -> Result<PathBuf> {
    if let Some(custom) = read_env_var_os(SESSION_DIR_ENV) {
        let path = PathBuf::from(custom);
        VtCodePaths::ensure_user_dir(&path)
            .with_context(|| format!("failed to initialize session directory {}", path.display()))?;
        return Ok(path);
    }

    let paths = VtCodePaths::resolve().context("failed to resolve VT Code paths")?;
    paths
        .ensure_state_dir()
        .context("failed to initialize VT Code state directory")?;
    let dir = paths.state_path("sessions")?;
    VtCodePaths::ensure_user_dir(&dir)
        .with_context(|| format!("failed to initialize session directory {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
#[path = "session_archive_tests.rs"]
mod tests;
