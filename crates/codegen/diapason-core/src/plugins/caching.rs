//! Plugin caching system for Diapason
//!
//! Implements the caching mechanism for plugins to ensure security and verification
//! as described in the Diapason plugin reference.

use hashbrown::HashMap;
use std::path::{Path, PathBuf};

use diapason_commons::DiapasonPaths;
use diapason_commons::fs::write_private_file_atomic;
use tokio::fs;

use super::{PluginError, PluginResult};

/// Plugin cache manager
pub struct PluginCache {
    /// Base directory for the plugin cache
    cache_dir: PathBuf,
    /// Mapping of plugin IDs to their cached paths
    cached_plugins: HashMap<String, PathBuf>,
}

impl PluginCache {
    /// Create a new plugin cache
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir, cached_plugins: HashMap::new() }
    }

    /// Cache a plugin from its source path
    pub async fn cache_plugin(&mut self, plugin_id: &str, source_path: &Path) -> PluginResult<PathBuf> {
        super::validate_plugin_component(plugin_id)?;

        // Validate source path exists
        let source_metadata = fs::symlink_metadata(source_path).await.map_err(|error| {
            PluginError::LoadingError(format!("Failed to inspect source path {}: {error}", source_path.display()))
        })?;
        if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
            return Err(PluginError::LoadingError(format!(
                "Plugin source is not a directory: {}",
                source_path.display()
            )));
        }

        // Create cache directory if it doesn't exist
        DiapasonPaths::ensure_user_dir(&self.cache_dir)
            .map_err(|e| PluginError::LoadingError(format!("Failed to create cache directory: {e}")))?;

        // Create plugin-specific cache directory
        let cache_path = self.cache_dir.join(plugin_id);

        // Remove existing cache if it exists
        if let Ok(metadata) = fs::symlink_metadata(&cache_path).await {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(PluginError::LoadingError(format!(
                    "Refusing to replace non-directory plugin cache path: {}",
                    cache_path.display()
                )));
            }
            fs::remove_dir_all(&cache_path)
                .await
                .map_err(|e| PluginError::LoadingError(format!("Failed to remove existing cache: {e}")))?;
        }

        // Copy plugin to cache directory
        self.copy_plugin_to_cache(source_path, &cache_path).await?;

        // Store in cache mapping
        self.cached_plugins.insert(plugin_id.to_string(), cache_path.clone());

        Ok(cache_path)
    }

    /// Copy plugin files to cache directory
    async fn copy_plugin_to_cache(&self, source: &Path, destination: &Path) -> PluginResult<()> {
        Box::pin(async {
            DiapasonPaths::ensure_user_dir(destination)
                .map_err(|e| PluginError::LoadingError(format!("Failed to create destination directory: {e}")))?;

            let mut entries = fs::read_dir(source)
                .await
                .map_err(|e| PluginError::LoadingError(format!("Failed to read source directory: {e}")))?;

            while let Some(entry) = entries
                .next_entry()
                .await
                .map_err(|e| PluginError::LoadingError(format!("Failed to read directory entry: {e}")))?
            {
                let src_path = entry.path();
                let dst_path = destination.join(entry.file_name());

                let source_metadata = fs::symlink_metadata(&src_path).await.map_err(|error| {
                    PluginError::LoadingError(format!("Failed to inspect plugin source entry: {error}"))
                })?;
                if source_metadata.file_type().is_symlink() {
                    return Err(PluginError::LoadingError(format!(
                        "Refusing to copy symlinked plugin entry: {}",
                        src_path.display()
                    )));
                }
                if source_metadata.is_dir() {
                    self.copy_plugin_to_cache(&src_path, &dst_path).await?;
                } else if source_metadata.is_file() {
                    if let Ok(destination_metadata) = fs::symlink_metadata(&dst_path).await
                        && destination_metadata.file_type().is_symlink()
                    {
                        return Err(PluginError::LoadingError(format!(
                            "Refusing to replace symlinked plugin cache entry: {}",
                            dst_path.display()
                        )));
                    }
                    let source = src_path.clone();
                    let contents = tokio::task::spawn_blocking(move || DiapasonPaths::read_file_no_follow(&source))
                        .await
                        .map_err(|error| {
                            PluginError::LoadingError(format!("Plugin source read task panicked: {error}"))
                        })?
                        .map_err(|error| PluginError::LoadingError(format!("Failed to read plugin file: {error}")))?;
                    write_private_file_atomic(&dst_path, contents)
                        .await
                        .map_err(|e| PluginError::LoadingError(format!("Failed to copy file: {e}")))?;
                } else {
                    return Err(PluginError::LoadingError(format!(
                        "Refusing to copy special plugin entry: {}",
                        src_path.display()
                    )));
                }
            }

            Ok(())
        })
        .await
    }

    /// Get cached plugin path
    pub fn get_cached_plugin(&self, plugin_id: &str) -> Option<&PathBuf> {
        self.cached_plugins.get(plugin_id)
    }
}
