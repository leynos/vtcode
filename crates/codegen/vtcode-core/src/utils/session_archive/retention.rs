//! Best-effort retention policy for durable session archives.

use super::{SESSION_MAX_AGE_DAYS_ENV, SESSION_MAX_FILES_ENV, SESSION_MAX_SIZE_MB_ENV, is_session_file, read_env_var};
use crate::core::SECONDS_PER_DAY;
use anyhow::{Result, anyhow};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_SESSION_MAX_FILES: usize = 100;
const DEFAULT_SESSION_MAX_AGE_DAYS: u64 = 14;
const DEFAULT_SESSION_MAX_SIZE_MB: u64 = 100;
pub(in crate::utils) const BYTES_PER_MB: u64 = 1024 * 1024;

#[derive(Debug, Clone)]
struct SessionFileEntry {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::utils) struct SessionRetentionLimits {
    pub(in crate::utils) max_files: usize,
    pub(in crate::utils) max_age_days: u64,
    pub(in crate::utils) max_total_size_bytes: u64,
}

impl Default for SessionRetentionLimits {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_SESSION_MAX_FILES,
            max_age_days: DEFAULT_SESSION_MAX_AGE_DAYS,
            max_total_size_bytes: DEFAULT_SESSION_MAX_SIZE_MB.saturating_mul(BYTES_PER_MB),
        }
    }
}

impl SessionRetentionLimits {
    fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            max_files: parse_env_value(SESSION_MAX_FILES_ENV).unwrap_or(defaults.max_files),
            max_age_days: parse_env_value(SESSION_MAX_AGE_DAYS_ENV).unwrap_or(defaults.max_age_days),
            max_total_size_bytes: parse_env_value::<u64>(SESSION_MAX_SIZE_MB_ENV)
                .map(|value| value.saturating_mul(BYTES_PER_MB))
                .unwrap_or(defaults.max_total_size_bytes),
        }
    }
}

fn session_retention_limits() -> SessionRetentionLimits {
    SessionRetentionLimits::from_env()
}

fn parse_env_value<T>(key: &str) -> Option<T>
where
    T: FromStr,
{
    read_env_var(key)?.trim().parse().ok()
}

pub(in crate::utils) fn apply_session_retention_best_effort(sessions_dir: &Path) {
    if apply_session_retention(sessions_dir).is_err() {
        tracing::warn!(
            operation = "session_archive_retention",
            outcome = "failed",
            error_category = "session_directory_read_failed",
            "Failed to prune session archives"
        );
    }
}

fn apply_session_retention(sessions_dir: &Path) -> Result<()> {
    apply_session_retention_with_limits(sessions_dir, session_retention_limits())
}

pub(in crate::utils) fn apply_session_retention_with_limits(
    sessions_dir: &Path,
    limits: SessionRetentionLimits,
) -> Result<()> {
    let entries = collect_session_entries(sessions_dir)?;

    if entries.is_empty() {
        return Ok(());
    }

    let entries = retain_session_archives_within_age(entries, limits.max_age_days, SystemTime::now());
    let entries = retain_most_recent_session_archives(entries, limits.max_files);
    remove_session_archives_over_size_limit(entries, limits.max_total_size_bytes);
    Ok(())
}

fn retain_session_archives_within_age(
    entries: Vec<SessionFileEntry>,
    max_age_days: u64,
    now: SystemTime,
) -> Vec<SessionFileEntry> {
    let age_cutoff = if max_age_days == 0 {
        now
    } else {
        now.checked_sub(Duration::from_secs(max_age_days.saturating_mul(SECONDS_PER_DAY)))
            .unwrap_or(UNIX_EPOCH)
    };

    let (expired, retained): (Vec<_>, Vec<_>) = entries.into_iter().partition(|entry| entry.modified <= age_cutoff);
    remove_session_files(expired);
    retained
}

fn retain_most_recent_session_archives(mut entries: Vec<SessionFileEntry>, max_files: usize) -> Vec<SessionFileEntry> {
    entries.sort_by(|a, b| b.modified.cmp(&a.modified));

    if max_files > 0 && entries.len() > max_files {
        let overflow = entries.split_off(max_files);
        remove_session_files(overflow);
    }

    entries
}

fn remove_session_archives_over_size_limit(entries: Vec<SessionFileEntry>, max_total_size_bytes: u64) {
    if max_total_size_bytes == 0 || entries.is_empty() {
        return;
    }

    let mut total_size = 0u64;
    let mut keep_entries = Vec::with_capacity(entries.len());
    let mut size_overflow = Vec::new();

    for entry in entries {
        let projected = total_size.saturating_add(entry.size);
        if keep_entries.is_empty() || projected <= max_total_size_bytes {
            total_size = projected;
            keep_entries.push(entry);
        } else {
            size_overflow.push(entry);
        }
    }

    remove_session_files(size_overflow);
}

fn collect_session_entries(sessions_dir: &Path) -> Result<Vec<SessionFileEntry>> {
    if !sessions_dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(sessions_dir)
        .map_err(|_error| anyhow!("failed to read session directory for retention (session_directory_read_failed)"))?
    {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(
                    operation = "session_archive_retention",
                    outcome = "entry_skipped",
                    error_category = "session_entry_read_failed",
                    "Failed to read a session archive entry"
                );
                continue;
            }
        };
        let path = entry.path();
        if !is_session_file(&path) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(value) => value,
            Err(_) => {
                tracing::warn!(
                    operation = "session_archive_retention",
                    outcome = "archive_skipped",
                    error_category = "session_archive_metadata_read_failed",
                    "Failed to read session archive metadata"
                );
                continue;
            }
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata.modified().unwrap_or(UNIX_EPOCH);
        entries.push(SessionFileEntry { path, modified, size: metadata.len() });
    }

    Ok(entries)
}

fn remove_session_files(entries: Vec<SessionFileEntry>) {
    for entry in entries {
        if fs::remove_file(&entry.path).is_err() {
            tracing::warn!(
                operation = "session_archive_retention",
                outcome = "remove_failed",
                error_category = "session_archive_remove_failed",
                "Failed to remove session archive"
            );
        }
    }
}
