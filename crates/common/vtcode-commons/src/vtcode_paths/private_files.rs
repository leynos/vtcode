//! Symlink-safe private filesystem primitives and bounded file locks.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use fs2::FileExt;

use super::{PRIVATE_FILE_LOCK_ATTEMPTS, PRIVATE_FILE_LOCK_DELAY};

pub(super) fn ensure_private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::other(format!("refusing symlink directory {}", path.display())));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::other(format!("{} is not a directory", path.display())));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_private_parent_dir(path)?;
            create_private_dir(path)?;
        }
        Err(error) => return Err(error),
    }
    set_private_permissions(path)
}

/// Ensure a user-owned directory exists without changing the mode of any
/// directory that was already present. Every directory created by this
/// helper is private on Unix.
pub(super) fn ensure_user_dir(path: &Path) -> io::Result<()> {
    validate_no_escaping_symlink_ancestors(path, true)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(io::Error::other(format!("refusing symlink directory {}", path.display())));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(io::Error::other(format!("{} is not a directory", path.display())));
        }
        Ok(_) => return Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let Some(parent) = path.parent() else {
        return Err(io::Error::other(format!("{} has no parent directory", path.display())));
    };
    if parent != path {
        ensure_user_dir(parent)?;
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(io::Error::other(format!("refusing symlink directory {}", path.display())))
        }
        Ok(metadata) if !metadata.is_dir() => Err(io::Error::other(format!("{} is not a directory", path.display()))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => create_user_dir(path),
        Err(error) => Err(error),
    }
}

fn ensure_private_parent_dir(path: &Path) -> io::Result<()> {
    validate_no_escaping_symlink_ancestors(path, true)?;
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent == path {
        return Ok(());
    }
    match fs::symlink_metadata(parent) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(io::Error::other(format!("refusing symlink directory {}", parent.display())))
        }
        Ok(metadata) if !metadata.is_dir() => Err(io::Error::other(format!("{} is not a directory", parent.display()))),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            ensure_private_parent_dir(parent)?;
            create_user_dir(parent)
        }
        Err(error) => Err(error),
    }
}

/// Ensure migration-created directories are private while preserving the mode
/// of directories that already existed at the destination.
pub(super) fn ensure_migration_dir(path: &Path) -> io::Result<()> {
    ensure_user_dir(path)
}

fn create_user_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => set_private_permissions(path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(io::Error::other(format!("refusing symlink directory {}", path.display())))
            }
            Ok(metadata) if !metadata.is_dir() => {
                Err(io::Error::other(format!("{} is not a directory", path.display())))
            }
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                Err(io::Error::other(format!("refusing symlink directory {}", path.display())))
            }
            Ok(metadata) if !metadata.is_dir() => {
                Err(io::Error::other(format!("{} is not a directory", path.display())))
            }
            Ok(_) => Ok(()),
            Err(error) => Err(error),
        },
        Err(error) => Err(error),
    }
}

pub(super) fn create_private_new_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    let _ = options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}
pub(super) fn open_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    let _ = options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

pub(super) fn open_private_append(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    let _ = options.create(true).append(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let _ = options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        let file = options.open(path)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        Ok(file)
    }
    #[cfg(not(unix))]
    options.open(path)
}

pub(super) fn ensure_file_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("file {} has no parent directory", path.display()))?;
    ensure_user_dir(parent).with_context(|| format!("could not create file parent {}", parent.display()))?;
    Ok(())
}

pub(super) fn validate_file_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!("refusing to replace symlinked file {}", path.display())
        }
        Ok(metadata) if !metadata.is_file() => bail!("{} is not a regular file", path.display()),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

/// Validate every existing path component and reject escaping symlinks.
///
/// Platform aliases such as macOS `/tmp -> /private/tmp` are accepted when a
/// symlink resolves beneath its containing directory. Missing trailing
/// components are allowed so callers can create them safely afterwards.
pub(super) fn validate_no_escaping_symlink_ancestors(path: &Path, allow_missing_tail: bool) -> io::Result<()> {
    let components = path.components().collect::<Vec<_>>();
    let mut current = PathBuf::new();
    for (index, component) in components.iter().enumerate() {
        if matches!(component, Component::ParentDir) {
            return Err(io::Error::other(format!("path contains traversal: {}", path.display())));
        }
        if matches!(component, Component::CurDir) {
            continue;
        }
        current.push(component.as_os_str());
        let is_leaf = index + 1 == components.len();
        let metadata = match fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if allow_missing_tail && error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_symlink() {
            if is_leaf {
                return Err(io::Error::other(format!("refusing symlink path {}", current.display())));
            }
            let parent = current
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            let canonical_parent = crate::canonicalize(parent)?;
            let canonical_target = crate::canonicalize(&current)?;
            if !canonical_target.starts_with(&canonical_parent) {
                return Err(io::Error::other(format!(
                    "path component {} escapes its containing directory",
                    current.display()
                )));
            }
            if !fs::metadata(&current)?.is_dir() {
                return Err(io::Error::other(format!("path component {} is not a directory", current.display())));
            }
        } else if !is_leaf && !metadata.is_dir() {
            return Err(io::Error::other(format!("path component {} is not a directory", current.display())));
        }
    }
    Ok(())
}

pub(super) fn unique_private_file(parent: &Path, stem: &OsStr) -> Result<(PathBuf, File)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let stem = stem.to_string_lossy();
    for attempt in 0..32u8 {
        let temporary = parent.join(format!(".{stem}.{}.{}.{}.tmp", std::process::id(), timestamp, attempt));
        match create_private_new_file(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error).with_context(|| format!("could not create {}", temporary.display())),
        }
    }
    bail!("could not allocate a unique private temporary file in {}", parent.display())
}

pub(super) struct PrivateFileLock {
    _file: File,
}

pub(super) fn acquire_private_file_lock(path: &Path) -> Result<PrivateFileLock> {
    let file = open_private_lock_file(path)?;
    for attempt in 0..PRIVATE_FILE_LOCK_ATTEMPTS {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(PrivateFileLock { _file: file }),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if attempt + 1 < PRIVATE_FILE_LOCK_ATTEMPTS {
                    std::thread::sleep(PRIVATE_FILE_LOCK_DELAY);
                }
            }
            Err(error) => return Err(error).with_context(|| format!("could not lock {}", path.display())),
        }
    }
    bail!("timed out waiting for private file lock {}", path.display())
}

fn open_private_lock_file(path: &Path) -> Result<File> {
    match create_private_lock_file(path) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_file_destination(path)?;
            let mut options = OpenOptions::new();
            let _ = options.read(true).write(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                let _ = options.custom_flags(libc::O_NOFOLLOW);
            }
            options
                .open(path)
                .with_context(|| format!("could not open private lock {}", path.display()))
        }
        Err(error) => Err(error).with_context(|| format!("could not create lock {}", path.display())),
    }
}

fn create_private_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    let _ = options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        let _ = options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

impl Drop for PrivateFileLock {
    fn drop(&mut self) {
        if let Err(error) = self._file.unlock() {
            tracing::debug!(%error, "failed to release private file lock");
        }
    }
}

pub(super) fn remove_temporary_file(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::debug!(path = %path.display(), %error, "failed to remove private temporary file");
    }
}
fn set_private_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
