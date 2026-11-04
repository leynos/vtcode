//! Models cache for persisting model metadata across sessions.
//!
//! This module provides TTL-based caching for model information,
//! following the pattern from OpenAI Codex's models_manager.

use chrono::{DateTime, Utc};
use diapason_commons::DiapasonPaths;
use diapason_commons::fs::{read_private_file_no_follow, with_private_file_lock};
use serde::{Deserialize, Serialize};
use std::io::{self, ErrorKind};
use std::path::Path;
use std::time::Duration;

use super::model_presets::ModelInfo;

/// Serialized snapshot of models and metadata cached on disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelsCache {
    /// Timestamp when the cache was last fetched
    pub fetched_at: DateTime<Utc>,
    /// ETag for conditional requests (if provider supports it)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// Provider this cache belongs to (e.g., "gemini", "openai")
    pub provider: String,
    /// Cached model information
    pub models: Vec<ModelInfo>,
}

impl ModelsCache {
    /// Create a new cache entry
    pub fn new(provider: impl Into<String>, models: Vec<ModelInfo>) -> Self {
        Self {
            fetched_at: Utc::now(),
            etag: None,
            provider: provider.into(),
            models,
        }
    }

    /// Create a new cache entry with an ETag
    pub fn with_etag(provider: impl Into<String>, models: Vec<ModelInfo>, etag: String) -> Self {
        Self {
            fetched_at: Utc::now(),
            etag: Some(etag),
            provider: provider.into(),
            models,
        }
    }

    /// Returns `true` when the cache entry has not exceeded the configured TTL.
    pub fn is_fresh(&self, ttl: Duration) -> bool {
        if ttl.is_zero() {
            return false;
        }
        let Ok(ttl_duration) = chrono::Duration::from_std(ttl) else {
            return false;
        };
        let age = Utc::now().signed_duration_since(self.fetched_at);
        age <= ttl_duration
    }

    /// Get the age of the cache entry
    pub fn age(&self) -> chrono::Duration {
        Utc::now().signed_duration_since(self.fetched_at)
    }
}

/// Read and deserialize the cache file if it exists.
pub async fn load_cache(path: &Path) -> io::Result<Option<ModelsCache>> {
    match read_private_file_no_follow(path).await {
        Ok(contents) => serde_json::from_slice(&contents)
            .map(Some)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err)),
        Err(err) => match err.downcast_ref::<io::Error>() {
            Some(io_err) if io_err.kind() == ErrorKind::NotFound => Ok(None),
            _ => Err(io::Error::other(err.to_string())),
        },
    }
}

/// Persist the cache contents to disk, creating parent directories as needed.
pub async fn save_cache(path: &Path, cache: &ModelsCache) -> io::Result<()> {
    let serialized = serde_json::to_vec_pretty(cache).map_err(|err| io::Error::other(err.to_string()))?;
    let lock_path = path.to_path_buf();
    let destination = path.to_path_buf();
    with_private_file_lock(&lock_path, move || DiapasonPaths::write_private_file_atomic(&destination, &serialized))
        .await
        .map_err(|err| io::Error::other(err.to_string()))
}

/// Publish a cache only when its canonical file is still absent.
pub async fn save_cache_if_absent(path: &Path, cache: &ModelsCache) -> io::Result<bool> {
    let serialized = serde_json::to_vec_pretty(cache).map_err(|err| io::Error::other(err.to_string()))?;
    let lock_path = path.to_path_buf();
    let destination = path.to_path_buf();
    with_private_file_lock(&lock_path, move || {
        DiapasonPaths::write_private_file_atomic_if_absent(&destination, &serialized)
    })
    .await
    .map_err(|err| io::Error::other(err.to_string()))
}

/// Load cache synchronously (for initialization)
pub fn load_cache_sync(path: &Path) -> io::Result<Option<ModelsCache>> {
    match DiapasonPaths::read_file_no_follow(path) {
        Ok(contents) => serde_json::from_slice(&contents)
            .map(Some)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err)),
        Err(err) => match err.downcast_ref::<io::Error>() {
            Some(io_err) if io_err.kind() == ErrorKind::NotFound => Ok(None),
            _ => Err(io::Error::other(err.to_string())),
        },
    }
}

/// Save cache synchronously
pub fn save_cache_sync(path: &Path, cache: &ModelsCache) -> io::Result<()> {
    let serialized = serde_json::to_vec_pretty(cache).map_err(|err| io::Error::other(err.to_string()))?;
    DiapasonPaths::with_private_file_lock(path, || DiapasonPaths::write_private_file_atomic(path, &serialized))
        .map_err(|err| io::Error::other(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn cache_is_fresh_when_within_ttl() {
        let cache = ModelsCache::new("test", vec![]);
        assert!(cache.is_fresh(Duration::from_secs(300)));
    }

    #[test]
    fn cache_is_stale_when_ttl_is_zero() {
        let cache = ModelsCache::new("test", vec![]);
        assert!(!cache.is_fresh(Duration::ZERO));
    }

    #[tokio::test]
    async fn cache_round_trips_through_disk() {
        let dir = tempdir().expect("create temp dir");
        let cache_path = dir.path().join("models_cache.json");

        let original = ModelsCache::new("gemini", vec![]);
        save_cache(&cache_path, &original).await.expect("save succeeds");

        let loaded = load_cache(&cache_path).await.expect("load succeeds").expect("cache exists");

        assert_eq!(loaded.provider, original.provider);
        assert_eq!(loaded.models.len(), original.models.len());
    }

    #[tokio::test]
    async fn load_returns_none_for_missing_file() {
        let dir = tempdir().expect("create temp dir");
        let cache_path = dir.path().join("nonexistent.json");

        let result = load_cache(&cache_path).await.expect("load succeeds");
        assert!(result.is_none());
    }
}
