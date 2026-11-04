use anyhow::{Result, anyhow};
use diapason_commons::DiapasonPaths;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::path::PathBuf;

use super::super::install_support::{
    acquire_lock_file, cache_is_stale, load_json_cache_with_legacy_fallback, lock_is_active, save_json_cache,
    unix_timestamp_now,
};

const INSTALL_LOCK_MAX_AGE_SECS: u64 = 1_800;
const INSTALL_CACHE_STALE_AFTER_SECS: u64 = 86_400;

/// Installation attempt cache to avoid repeated retries
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct InstallationCache {
    /// Timestamp of last installation attempt
    last_attempt: u64,
    /// Status from last attempt
    pub(super) status: String,
    /// Method that was attempted
    method_attempted: Option<String>,
    /// Reason for failure (if applicable)
    pub(super) failure_reason: Option<String>,
}

#[derive(Debug)]
pub(super) struct InstallLockGuard {
    path: PathBuf,
    _file: File,
}

impl InstallationCache {
    fn state_dir() -> Result<PathBuf> {
        DiapasonPaths::resolve()?.ensure_cache_child_dir("ripgrep")
    }

    fn cache_path() -> Result<PathBuf> {
        Self::cache_paths().map(|(canonical, _)| canonical)
    }

    fn cache_paths() -> Result<(PathBuf, Vec<PathBuf>)> {
        let paths = DiapasonPaths::resolve()?;
        paths.ensure_cache_child_dir("ripgrep")?;
        let canonical = paths.cache_path("ripgrep/ripgrep_install_cache.json")?;
        Ok((canonical, Vec::new()))
    }

    pub(super) fn is_stale() -> bool {
        match Self::load() {
            Ok(cache) => cache_is_stale(cache.last_attempt, INSTALL_CACHE_STALE_AFTER_SECS),
            Err(_) => true,
        }
    }

    pub(super) fn load() -> Result<Self> {
        let (canonical, legacy) = Self::cache_paths()?;
        load_json_cache_with_legacy_fallback(&canonical, &legacy, "ripgrep installation cache")
    }

    fn save(&self) -> Result<()> {
        let state_dir = Self::state_dir()?;
        let cache_path = Self::cache_path()?;
        save_json_cache(&state_dir, &cache_path, self, "ripgrep installation cache")
    }

    pub(super) fn mark_failed(method: &str, reason: &str) {
        let cache = InstallationCache {
            last_attempt: unix_timestamp_now(),
            status: "failed".to_string(),
            method_attempted: Some(method.to_string()),
            failure_reason: Some(reason.to_string()),
        };
        let _ = cache.save();
    }

    pub(super) fn mark_success(method: &str) {
        let cache = InstallationCache {
            last_attempt: unix_timestamp_now(),
            status: "success".to_string(),
            method_attempted: Some(method.to_string()),
            failure_reason: None,
        };
        let _ = cache.save();
    }
}

impl InstallLockGuard {
    pub(super) fn acquire() -> Result<Self> {
        let path = lock_path()?;
        match acquire_lock_file(&path, INSTALL_LOCK_MAX_AGE_SECS)? {
            Some(file) => Ok(Self { path, _file: file }),
            None => Err(anyhow!("Ripgrep installation already in progress")),
        }
    }

    pub(super) fn is_install_in_progress() -> bool {
        lock_path().is_ok_and(|path| lock_is_active(&path, INSTALL_LOCK_MAX_AGE_SECS))
    }
}

impl Drop for InstallLockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_path() -> Result<PathBuf> {
    DiapasonPaths::resolve()?
        .ensure_runtime_child_dir("ripgrep")
        .map(|path| path.join("install.lock"))
}
