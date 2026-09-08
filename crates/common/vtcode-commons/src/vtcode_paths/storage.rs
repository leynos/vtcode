//! Secure storage operations and legacy migration entry points.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

use super::private_files::{
    acquire_private_file_lock, create_private_new_file, ensure_file_parent, ensure_private_dir, ensure_user_dir,
    open_no_follow, open_private_append, remove_temporary_file, unique_private_file, validate_file_destination,
    validate_no_escaping_symlink_ancestors,
};
use super::{LegacyMigrator, MigrationReport, VtCodePaths};

impl VtCodePaths {
    /// Creates the runtime directory with private permissions.
    ///
    /// # Errors
    /// Returns an error when the private runtime directory cannot be created or secured.
    pub fn ensure_runtime_dir(&self) -> Result<&Path> {
        ensure_private_dir(&self.runtime_dir).context("could not create VT Code runtime directory")?;
        Ok(&self.runtime_dir)
    }

    /// Creates the canonical user configuration directory when needed.
    /// Existing directories retain their permissions.
    ///
    /// # Errors
    /// Returns an error when the corresponding user directory cannot be created safely.
    pub fn ensure_config_dir(&self) -> Result<&Path> {
        ensure_user_dir(&self.config_dir).context("could not create VT Code configuration directory")?;
        Ok(&self.config_dir)
    }

    /// Creates the canonical user data directory when needed.
    /// Existing directories retain their permissions.
    ///
    /// # Errors
    /// Returns an error when the corresponding user directory cannot be created safely.
    pub fn ensure_data_dir(&self) -> Result<&Path> {
        ensure_user_dir(&self.data_dir).context("could not create VT Code data directory")?;
        Ok(&self.data_dir)
    }

    /// Creates the canonical user state directory when needed.
    /// Existing directories retain their permissions.
    ///
    /// # Errors
    /// Returns an error when the corresponding user directory cannot be created safely.
    pub fn ensure_state_dir(&self) -> Result<&Path> {
        ensure_user_dir(&self.state_dir).context("could not create VT Code state directory")?;
        Ok(&self.state_dir)
    }

    /// Creates the canonical user cache directory when needed.
    /// Existing directories retain their permissions.
    ///
    /// # Errors
    /// Returns an error when the corresponding user directory cannot be created safely.
    pub fn ensure_cache_dir(&self) -> Result<&Path> {
        ensure_user_dir(&self.cache_dir).context("could not create VT Code cache directory")?;
        Ok(&self.cache_dir)
    }

    /// Creates the managed executable directory when needed.
    /// Existing directories retain their permissions.
    ///
    /// # Errors
    /// Returns an error when the corresponding user directory cannot be created safely.
    pub fn ensure_executable_dir(&self) -> Result<&Path> {
        ensure_user_dir(&self.executable_dir).context("could not create VT Code executable directory")?;
        Ok(&self.executable_dir)
    }

    /// Creates an arbitrary user-owned directory without following symlinks.
    /// Existing directories retain their permissions; newly created Unix
    /// directories use mode `0700`.
    ///
    /// # Errors
    /// Returns an error when the requested user directory cannot be created safely.
    pub fn ensure_user_dir(path: impl AsRef<Path>) -> Result<PathBuf> {
        let path = path.as_ref();
        ensure_user_dir(path).with_context(|| format!("could not create user directory {}", path.display()))?;
        Ok(path.to_path_buf())
    }

    /// Creates a new private file without following a final symlink.
    ///
    /// The parent is validated component by component before the file is
    /// opened. This is the common primitive for caches, locks, and other
    /// category-owned files that must never be redirected through a symlink.
    ///
    /// # Errors
    /// Returns an error when the path fails no-follow validation or the file operation fails.
    pub fn create_private_file(path: impl AsRef<Path>) -> Result<File> {
        let path = path.as_ref();
        ensure_file_parent(path)?;
        create_private_new_file(path).with_context(|| format!("could not create private file {}", path.display()))
    }

    /// Opens an append-only private file without following a final symlink.
    ///
    /// # Errors
    /// Returns an error when the path fails no-follow validation or the file operation fails.
    pub fn open_private_append_file(path: impl AsRef<Path>) -> Result<File> {
        let path = path.as_ref();
        ensure_file_parent(path)?;
        open_private_append(path).with_context(|| format!("could not open private file {}", path.display()))
    }

    /// Reads a regular file without following a final symlink.
    ///
    /// # Errors
    /// Returns an error when the path fails no-follow validation or the file operation fails.
    pub fn read_file_no_follow(path: impl AsRef<Path>) -> Result<Vec<u8>> {
        let path = path.as_ref();
        validate_no_escaping_symlink_ancestors(path, false)
            .with_context(|| format!("could not validate file path {}", path.display()))?;
        let mut file = open_no_follow(path).with_context(|| format!("could not open file {}", path.display()))?;
        let metadata = file
            .metadata()
            .with_context(|| format!("could not inspect file {}", path.display()))?;
        if !metadata.is_file() {
            bail!("{} is not a regular file", path.display());
        }
        let mut contents = Vec::new();
        let _bytes_read = file
            .read_to_end(&mut contents)
            .with_context(|| format!("could not read file {}", path.display()))?;
        Ok(contents)
    }

    /// Atomically writes a private file, replacing an existing regular file.
    ///
    /// The destination is never opened for writing. A private, exclusive
    /// temporary file is written and then renamed into place, so a pre-existing
    /// symlink cannot redirect the contents outside the approved parent.
    ///
    /// # Errors
    /// Returns an error when the destination fails validation or atomic publication fails.
    pub fn write_private_file_atomic(path: impl AsRef<Path>, contents: &[u8]) -> Result<()> {
        let destination = path.as_ref();
        ensure_file_parent(destination)?;
        validate_file_destination(destination)?;
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("private file {} has no parent", destination.display()))?;
        let stem = destination.file_name().unwrap_or_else(|| OsStr::new("file"));
        let (temporary, mut file) = unique_private_file(parent, stem)?;
        let result: io::Result<()> = (|| {
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            #[cfg(windows)]
            if fs::symlink_metadata(destination).is_ok() {
                fs::remove_file(destination)?;
            }
            fs::rename(&temporary, destination)
        })();
        if result.is_err() {
            remove_temporary_file(&temporary);
        }
        result.with_context(|| format!("could not atomically write {}", destination.display()))
    }

    /// Atomically publishes a private file only when the destination is absent.
    ///
    /// The temporary file is linked into place instead of renamed over the
    /// destination. This makes legacy-cache republishing safe when multiple
    /// VT Code processes initialize the same cache concurrently: the first
    /// publisher wins and a newer canonical cache cannot be clobbered.
    ///
    /// # Errors
    /// Returns an error when the destination fails validation or atomic publication fails.
    pub fn write_private_file_atomic_if_absent(path: impl AsRef<Path>, contents: &[u8]) -> Result<bool> {
        let destination = path.as_ref();
        ensure_file_parent(destination)?;
        validate_file_destination(destination)?;
        if fs::symlink_metadata(destination).is_ok() {
            return Ok(false);
        }

        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("private file {} has no parent", destination.display()))?;
        let stem = destination.file_name().unwrap_or_else(|| OsStr::new("file"));
        let (temporary, mut file) = unique_private_file(parent, stem)?;
        let result: io::Result<bool> = (|| {
            file.write_all(contents)?;
            file.sync_all()?;
            drop(file);
            match fs::hard_link(&temporary, destination) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(error) => Err(error),
            }
        })();
        remove_temporary_file(&temporary);
        result.with_context(|| format!("could not atomically create {}", destination.display()))
    }

    /// Runs an operation while holding an exclusive private lock adjacent to a file.
    ///
    /// Lock acquisition is bounded and released by the operating system when
    /// the owning process exits, so an abandoned lock cannot permanently
    /// disable cache writes.
    ///
    /// # Errors
    /// Returns an error when the private lock cannot be acquired or the operation fails.
    pub fn with_private_file_lock<T>(path: impl AsRef<Path>, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let destination = path.as_ref();
        ensure_file_parent(destination)?;
        let parent = destination
            .parent()
            .ok_or_else(|| anyhow!("private file {} has no parent", destination.display()))?;
        let stem = destination.file_name().unwrap_or_else(|| OsStr::new("file"));
        let lock_path = parent.join(format!(".{}.lock", stem.to_string_lossy()));
        let _lock = acquire_private_file_lock(&lock_path)?;
        operation()
    }

    /// Creates a private runtime child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_runtime_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.runtime_path(relative)?;
        ensure_private_dir(&path).context("could not create VT Code runtime child directory")?;
        Ok(path)
    }

    /// Creates a user configuration child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_config_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.config_path(relative)?;
        ensure_user_dir(&path).context("could not create VT Code configuration child directory")?;
        Ok(path)
    }

    /// Creates a user data child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_data_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.data_path(relative)?;
        ensure_user_dir(&path).context("could not create VT Code data child directory")?;
        Ok(path)
    }

    /// Creates a user state child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_state_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.state_path(relative)?;
        ensure_user_dir(&path).context("could not create VT Code state child directory")?;
        Ok(path)
    }

    /// Creates a user cache child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_cache_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.cache_path(relative)?;
        ensure_user_dir(&path).context("could not create VT Code cache child directory")?;
        Ok(path)
    }

    /// Creates a managed executable child directory after validating its path.
    ///
    /// # Errors
    /// Returns an error when the relative child path is invalid or its directory cannot be created safely.
    pub fn ensure_executable_child_dir(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.executable_path(relative)?;
        ensure_user_dir(&path).context("could not create VT Code executable child directory")?;
        Ok(path)
    }

    /// Creates private auth storage and returns its path.
    ///
    /// # Errors
    /// Returns an error when authentication storage cannot be validated or created safely.
    pub fn ensure_auth_dir(&self) -> Result<PathBuf> {
        let path = self.auth_dir();
        ensure_private_dir(&path).context("could not create VT Code authentication directory")?;
        Ok(path)
    }

    /// Creates an empty, private auth file. The name must not contain a path.
    ///
    /// # Errors
    /// Returns an error when authentication storage cannot be validated or created safely.
    pub fn create_auth_file(&self, name: impl AsRef<str>) -> Result<PathBuf> {
        let name = name.as_ref();
        if !is_safe_file_name(name) {
            bail!("authentication file name '{name}' must be one normal path component");
        }
        let path = self.ensure_auth_dir()?.join(name);
        let _file = create_private_new_file(&path)
            .with_context(|| format!("could not create authentication file {}", path.display()))?;
        Ok(path)
    }

    /// Copies eligible legacy global data without modifying its source.
    ///
    /// # Errors
    /// Returns an error only when migration initialization cannot establish its marker parent.
    pub fn migrate_legacy(&self) -> Result<MigrationReport> {
        LegacyMigrator::new(self.clone()).run()
    }
}
fn is_safe_file_name(name: &str) -> bool {
    let mut parts = Path::new(name).components();
    matches!(parts.next(), Some(Component::Normal(_))) && parts.next().is_none()
}
