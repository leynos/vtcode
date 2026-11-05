//! Typed path accessors and child-path resolution.

use std::path::{Component, Path, PathBuf};

use anyhow::{Result, bail};

use super::{APP, MARKER, VtCodePaths};

impl VtCodePaths {
    /// Canonical directory for user configuration.
    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }
    /// Canonical directory for durable user data.
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }
    /// Canonical directory for durable mutable state.
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }
    /// Canonical directory for recreatable cached data.
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }
    /// Private directory for runtime-only files.
    pub fn runtime_dir(&self) -> &Path {
        &self.runtime_dir
    }
    /// User executable helper directory.
    pub fn executable_dir(&self) -> &Path {
        &self.executable_dir
    }
    /// System configuration candidates, lowest priority first.
    pub fn system_config_dirs(&self) -> &[PathBuf] {
        &self.system_config_dirs
    }
    /// System data candidates without an application suffix.
    pub fn system_data_dirs(&self) -> &[PathBuf] {
        &self.system_data_dirs
    }
    /// Resolves a relative config path against all system candidates in
    /// low-to-high precedence order. `system_config_dirs()` retains the XDG
    /// preference order, where the first directory is most important.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn system_config_paths(&self, relative: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let relative = relative.as_ref();
        validate_relative_path("system configuration path", relative)?;
        let mut paths = if cfg!(unix) {
            vec![PathBuf::from("/etc/vtcode").join(relative)]
        } else {
            Vec::new()
        };
        paths.extend(self.system_config_dirs.iter().rev().map(|base| base.join(APP).join(relative)));
        paths.dedup();
        Ok(paths)
    }
    /// Resolves a relative data path against all system candidates.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn system_data_paths(&self, relative: impl AsRef<Path>) -> Result<Vec<PathBuf>> {
        let relative = relative.as_ref();
        validate_relative_path("system data path", relative)?;
        Ok(self.system_data_dirs.iter().map(|base| base.join(APP).join(relative)).collect())
    }
    /// Legacy global VT Code root, used only as a migration source.
    pub fn legacy_home_dir(&self) -> &Path {
        &self.legacy_home_dir
    }

    /// Alias for [`Self::legacy_home_dir`] using the public contract name.
    pub fn legacy_dir(&self) -> &Path {
        self.legacy_home_dir()
    }

    /// Resolve a relative child path under the canonical configuration root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn config_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.config_dir, relative.as_ref(), "configuration")
    }

    /// Resolve a relative child path under the canonical data root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn data_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.data_dir, relative.as_ref(), "data")
    }

    /// Resolve a relative child path under the persistent state root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn state_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.state_dir, relative.as_ref(), "state")
    }

    /// Resolve a relative child path under the cache root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn cache_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.cache_dir, relative.as_ref(), "cache")
    }

    /// Resolve a relative child path under the runtime root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn runtime_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.runtime_dir, relative.as_ref(), "runtime")
    }

    /// Resolve a relative child path under the managed executable root.
    ///
    /// # Errors
    /// Returns an error when `relative` is empty, absolute, or contains traversal components.
    pub fn executable_path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        child_path(&self.executable_dir, relative.as_ref(), "executable")
    }

    /// Main user configuration file.
    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("vtcode.toml")
    }
    /// User-installed and downloaded skills.
    pub fn skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }
    /// User-installed plugins.
    pub fn plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }
    /// Private authentication storage.
    pub fn auth_dir(&self) -> PathBuf {
        self.config_dir.join("auth")
    }
    /// Legacy plaintext authentication file location.
    pub fn auth_file(&self) -> PathBuf {
        self.auth_dir().join("auth.json")
    }
    /// Durable log files.
    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir.join("logs")
    }
    /// Persisted cross-workspace session data.
    pub fn sessions_dir(&self) -> PathBuf {
        self.state_dir.join("sessions")
    }
    /// Telemetry storage.
    pub fn telemetry_dir(&self) -> PathBuf {
        self.state_dir.join("telemetry")
    }
    /// Completion marker for one-time legacy migration.
    pub fn migration_marker_path(&self) -> PathBuf {
        self.state_dir.join("migration").join(MARKER)
    }

    /// Versioned diagnostic report emitted alongside the completion marker.
    pub fn migration_report_path(&self) -> PathBuf {
        self.state_dir.join("migration").join("legacy-v1.json")
    }
}

fn validate_relative_path(name: &str, path: &Path) -> Result<()> {
    if !path.as_os_str().is_empty() && path.components().all(|component| matches!(component, Component::Normal(_))) {
        Ok(())
    } else {
        bail!("{name} must be a non-empty relative path without traversal, got '{}'", path.display())
    }
}

fn child_path(root: &Path, relative: &Path, category: &str) -> Result<PathBuf> {
    validate_relative_path(&format!("{category} child path"), relative)?;
    Ok(root.join(relative))
}
