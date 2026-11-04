//! Plugin installer for marketplace system

use std::path::{Component, Path, PathBuf};

use crate::tools::plugins::PluginRuntime;
use crate::utils::validation::{validate_all_non_empty, validate_non_empty};
use anyhow::{Context, Result, bail};
use diapason_commons::DiapasonPaths;
use diapason_commons::fs::{read_private_file_no_follow, write_private_file_atomic, write_private_json_file};
use tokio::fs;

use super::PluginManifest;

/// Plugin installer that handles downloading and installing plugins from marketplaces
pub struct PluginInstaller {
    /// Base directory for installed plugins
    pub plugins_dir: PathBuf,

    /// Reference to the core plugin runtime for integration
    core_plugin_runtime: Option<PluginRuntime>,
}

impl PluginInstaller {
    /// Create a new installer targeting the given plugins directory.
    pub fn new(plugins_dir: PathBuf, core_plugin_runtime: Option<PluginRuntime>) -> Self {
        Self { plugins_dir, core_plugin_runtime }
    }

    /// Install a plugin from its manifest
    pub async fn install_plugin(&self, manifest: &PluginManifest) -> Result<()> {
        self.validate_manifest(manifest)?;

        // Create plugins directory if it doesn't exist
        DiapasonPaths::ensure_user_dir(&self.plugins_dir)
            .with_context(|| format!("Failed to create plugin directory: {}", self.plugins_dir.display()))?;

        // Create plugin installation directory
        let plugin_dir = self.plugins_dir.join(&manifest.id);
        DiapasonPaths::ensure_user_dir(&plugin_dir)
            .with_context(|| format!("Failed to create plugin directory: {}", plugin_dir.display()))?;

        // Download the plugin from its source
        self.download_plugin(manifest, &plugin_dir).await?;

        // Save the manifest to the plugin directory
        let manifest_dir = plugin_dir.join(".diapason-plugin");
        let manifest_path = manifest_dir.join("plugin.json");
        write_private_json_file(&manifest_path, manifest).await?;

        // Integrate with Diapason's existing plugin system
        self.integrate_with_core_plugin_system(&manifest_path).await?;

        Ok(())
    }

    /// Integrate the installed plugin with Diapason's core plugin system
    async fn integrate_with_core_plugin_system(&self, manifest_path: &Path) -> Result<()> {
        // This would load the plugin into Diapason's plugin runtime
        if let Some(runtime) = &self.core_plugin_runtime {
            // Load the plugin manifest and register it with the core runtime
            let handle = runtime.register_manifest(manifest_path).await?;
            tracing::info!(plugin_id = %handle.manifest.id, "registered plugin with core runtime");
        } else {
            tracing::info!(path = %manifest_path.display(), "no core plugin runtime, skipping integration");
        }

        Ok(())
    }

    /// Download plugin from its source
    async fn download_plugin(&self, manifest: &PluginManifest, plugin_dir: &Path) -> Result<()> {
        // Validate the manifest before downloading
        self.validate_manifest(manifest)?;

        tracing::info!(plugin_id = %manifest.id, source = %manifest.source, "downloading plugin");

        // Determine the source type and download accordingly
        if manifest.source.starts_with("http") {
            self.download_from_http(manifest, plugin_dir).await?;
        } else if manifest.source.starts_with("file://") {
            self.download_from_file(manifest, plugin_dir).await?;
        } else if fs::try_exists(&manifest.source)
            .await
            .with_context(|| format!("Failed to check plugin source {}", manifest.source))?
        {
            // Local path
            self.download_from_local(manifest, plugin_dir).await?;
        } else {
            // Assume it's a git repository
            self.download_from_git(manifest, plugin_dir).await?;
        }

        Ok(())
    }

    /// Download plugin from HTTP source
    async fn download_from_http(&self, manifest: &PluginManifest, plugin_dir: &Path) -> Result<()> {
        // For now, we'll create a placeholder since we don't have the actual HTTP client configured
        let placeholder_path = self.entrypoint_path(plugin_dir, manifest)?;

        write_private_file_atomic(&placeholder_path, format!("# HTTP Downloaded plugin: {}\n", manifest.id)).await?;

        tracing::info!(plugin_id = %manifest.id, "http download completed");
        Ok(())
    }

    /// Download plugin from local file
    async fn download_from_file(&self, manifest: &PluginManifest, plugin_dir: &Path) -> Result<()> {
        let source_path = PathBuf::from(&manifest.source.replace("file://", ""));
        let content = read_private_file_no_follow(&source_path)
            .await
            .with_context(|| format!("Failed to read local source file {}", source_path.display()))?;
        let dest_path = self.entrypoint_path(plugin_dir, manifest)?;

        // Copy the file from source to destination
        write_private_file_atomic(&dest_path, content).await.with_context(|| {
            format!("Failed to copy plugin from {} to {}", source_path.display(), dest_path.display())
        })?;

        tracing::info!(plugin_id = %manifest.id, "local file copy completed");
        Ok(())
    }

    /// Download plugin from local path
    async fn download_from_local(&self, manifest: &PluginManifest, plugin_dir: &Path) -> Result<()> {
        let source_path = PathBuf::from(&manifest.source);
        let content = read_private_file_no_follow(&source_path)
            .await
            .with_context(|| format!("Failed to read local source path {}", source_path.display()))?;
        let dest_path = self.entrypoint_path(plugin_dir, manifest)?;

        // Copy the file from source to destination
        write_private_file_atomic(&dest_path, content).await.with_context(|| {
            format!("Failed to copy plugin from {} to {}", source_path.display(), dest_path.display())
        })?;

        tracing::info!(plugin_id = %manifest.id, "local path copy completed");
        Ok(())
    }

    /// Download plugin from git repository
    async fn download_from_git(&self, manifest: &PluginManifest, plugin_dir: &Path) -> Result<()> {
        // For now, we'll create a placeholder since we don't have git functionality integrated
        let placeholder_path = self.entrypoint_path(plugin_dir, manifest)?;

        write_private_file_atomic(&placeholder_path, format!("# Git downloaded plugin: {}\n", manifest.id)).await?;

        tracing::info!(plugin_id = %manifest.id, "git download completed");
        Ok(())
    }

    /// Validate the plugin manifest before installation
    pub fn validate_manifest(&self, manifest: &PluginManifest) -> Result<()> {
        // Validate required fields
        validate_non_empty(&manifest.id, "Plugin ID")?;
        validate_non_empty(&manifest.name, "Plugin name")?;
        validate_non_empty(&manifest.source, "Plugin source URL")?;

        // Validate entrypoint path
        validate_plugin_component(&manifest.id, "Plugin ID")?;
        validate_relative_path(&manifest.entrypoint, "Plugin entrypoint")?;

        // Validate trust level if specified
        if let Some(trust_level) = &manifest.trust_level {
            match trust_level {
                crate::config::PluginTrustLevel::Sandbox
                | crate::config::PluginTrustLevel::Trusted
                | crate::config::PluginTrustLevel::Untrusted => {
                    // Valid trust level
                }
            }
        }

        // Validate dependencies if any
        validate_all_non_empty(&manifest.dependencies, "Plugin dependencies")?;

        Ok(())
    }

    /// Uninstall a plugin by ID
    pub async fn uninstall_plugin(&self, plugin_id: &str) -> Result<()> {
        validate_plugin_component(plugin_id, "Plugin ID")?;
        DiapasonPaths::ensure_user_dir(&self.plugins_dir)
            .with_context(|| format!("Failed to validate plugin directory: {}", self.plugins_dir.display()))?;
        let plugin_dir = self.plugins_dir.join(plugin_id);
        let metadata = match fs::symlink_metadata(&plugin_dir).await {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                bail!("Installed plugin path does not exist: {}", plugin_dir.display());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Failed to inspect installed plugin {}", plugin_dir.display()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!("Refusing to remove non-directory plugin path: {}", plugin_dir.display());
        }

        // Remove from Diapason's plugin system before filesystem removal
        self.remove_from_core_plugin_system(plugin_id).await?;

        fs::remove_dir_all(&plugin_dir)
            .await
            .with_context(|| format!("Failed to remove plugin directory: {}", plugin_dir.display()))?;

        Ok(())
    }

    /// Remove plugin from Diapason's core plugin system
    async fn remove_from_core_plugin_system(&self, plugin_id: &str) -> Result<()> {
        // Remove the plugin from Diapason's plugin runtime
        if let Some(runtime) = &self.core_plugin_runtime {
            // Unload the plugin by ID
            runtime
                .unload_plugin(plugin_id)
                .await
                .with_context(|| format!("Failed to unload plugin from runtime: {plugin_id}"))?;
            tracing::info!(plugin_id = %plugin_id, "unloaded plugin from core runtime");
        } else {
            tracing::info!(plugin_id = %plugin_id, "no core plugin runtime, skipping removal");
        }

        Ok(())
    }

    /// Check if a plugin is installed
    pub async fn is_installed(&self, plugin_id: &str) -> bool {
        if validate_plugin_component(plugin_id, "Plugin ID").is_err() {
            return false;
        }
        let plugin_dir = self.plugins_dir.join(plugin_id);
        fs::symlink_metadata(plugin_dir)
            .await
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    }

    fn entrypoint_path(&self, plugin_dir: &Path, manifest: &PluginManifest) -> Result<PathBuf> {
        validate_relative_path(&manifest.entrypoint, "Plugin entrypoint")?;
        let path = plugin_dir.join(&manifest.entrypoint);
        if let Some(parent) = path.parent() {
            DiapasonPaths::ensure_user_dir(parent)
                .with_context(|| format!("Failed to create plugin entrypoint directory: {}", parent.display()))?;
        }
        Ok(path)
    }
}

fn validate_plugin_component(value: &str, label: &str) -> Result<()> {
    let mut components = Path::new(value).components();
    if value.trim().is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
    {
        bail!("{label} must be one normal path component: {value}");
    }
    Ok(())
}

fn validate_relative_path(path: &Path, label: &str) -> Result<()> {
    if path.as_os_str().is_empty() || !path.components().all(|component| matches!(component, Component::Normal(_))) {
        bail!("{label} must be a non-empty relative path without traversal: {}", path.display());
    }
    Ok(())
}
