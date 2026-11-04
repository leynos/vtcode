//! Shared resolution of configurable workspace data and XDG workspace state.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// Default directory for data that belongs inside a worktree.
pub const DEFAULT_WORKSPACE_DIR: &str = ".diapason";
/// Environment variable selecting a single worktree directory name.
pub const WORKSPACE_DIR_ENV: &str = "DIAPASON_WORKSPACE_DIR";

fn validated_name(name: &OsStr) -> Result<PathBuf> {
    let path = Path::new(name);
    let mut components = path.components();
    if name.is_empty()
        || !matches!(components.next(), Some(Component::Normal(_)))
        || components.next().is_some()
        || name.to_string_lossy().eq_ignore_ascii_case(".vtcode")
        || name.to_string_lossy().contains(['/', '\\'])
    {
        bail!("{WORKSPACE_DIR_ENV} must be one directory name and cannot select the upstream data directory");
    }
    Ok(path.to_path_buf())
}

/// Reject an invalid workspace directory override before application startup.
pub fn validate_workspace_dir_override() -> Result<()> {
    if let Some(name) = std::env::var_os(WORKSPACE_DIR_ENV) {
        let _validated_name = validated_name(&name)?;
    }
    Ok(())
}

/// Return the configured worktree directory name.
///
/// Startup must call [`validate_workspace_dir_override`] first. Library-only
/// consumers safely retain the default when handed an invalid override.
#[must_use]
pub fn workspace_dir_name() -> PathBuf {
    std::env::var_os(WORKSPACE_DIR_ENV)
        .and_then(|name| validated_name(&name).ok())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_WORKSPACE_DIR))
}

/// Resolve workspace data without following a directory link outside the workspace.
pub fn workspace_data_dir(workspace: &Path) -> Result<PathBuf> {
    validate_workspace_dir_override()?;
    workspace_data_dir_for_name(workspace, &workspace_dir_name())
}

fn workspace_data_dir_for_name(workspace: &Path, name: &Path) -> Result<PathBuf> {
    let candidate = workspace.join(name);
    let exists = match candidate.symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error).context("inspecting workspace data directory"),
    };
    if exists {
        let root = crate::canonicalize(workspace).context("canonicalizing workspace data root")?;
        let resolved = crate::canonicalize(&candidate).context("resolving workspace data directory")?;
        if !resolved.starts_with(&root) {
            bail!("workspace data directory escapes its workspace");
        }
    }
    Ok(candidate)
}

/// Resolve persisted state for a workspace beneath the user's XDG state root.
///
/// A stable digest keeps separate workspaces isolated without exposing their
/// absolute paths in directory names. Existing workspace symlinks share state.
/// Hash identity omits Windows verbatim namespace prefixes; canonical filesystem
/// paths retain their native representation for all I/O and containment checks.
pub fn workspace_state_dir(workspace: &Path) -> Result<PathBuf> {
    let canonical = crate::canonicalize(workspace)
        .or_else(|error| {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error);
            }
            if workspace.is_absolute() {
                Ok(workspace.to_path_buf())
            } else {
                std::env::current_dir().map(|cwd| cwd.join(workspace))
            }
        })
        .context("resolving workspace for persisted state")?;
    match std::fs::metadata(&canonical) {
        Ok(metadata) if !metadata.is_dir() => {
            bail!("workspace for persisted state is not a directory: {}", workspace.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).context("inspecting workspace for persisted state"),
    }
    let key = workspace_state_key(&canonical)?;
    Ok(crate::DiapasonPaths::resolve()?.state_dir().join("workspaces").join(key))
}

fn workspace_state_key(workspace: &Path) -> Result<String> {
    let identity = workspace_hash_identity_bytes(workspace.as_os_str().as_encoded_bytes());
    let digest = Sha256::digest(identity.as_ref());
    let mut key = String::with_capacity(digest.len() * 2);
    for byte in digest.iter() {
        write!(&mut key, "{byte:02x}").context("encoding workspace state digest")?;
    }
    Ok(key)
}

/// Normalize only the hash identity, never the path used for filesystem I/O.
/// Windows native encoded bytes are WTF-8; their non-prefix bytes stay exact.
fn workspace_hash_identity_bytes(path: &[u8]) -> Cow<'_, [u8]> {
    if let Some(remainder) = path.strip_prefix(br"\\?\UNC\") {
        let mut identity = Vec::with_capacity(remainder.len() + 2);
        identity.extend_from_slice(br"\\");
        identity.extend_from_slice(remainder);
        Cow::Owned(identity)
    } else if let Some(remainder) = path.strip_prefix(br"\\?\") {
        Cow::Borrowed(remainder)
    } else {
        Cow::Borrowed(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_hash_identity_removes_only_windows_verbatim_namespaces() {
        struct IdentityCase {
            native_path_bytes: &'static [u8],
            expected_identity_bytes: &'static [u8],
        }
        let case_rows = [
            IdentityCase {
                native_path_bytes: br"\\?\C:\repo",
                expected_identity_bytes: br"C:\repo",
            },
            IdentityCase {
                native_path_bytes: br"\\?\UNC\server\share\repo",
                expected_identity_bytes: br"\\server\share\repo",
            },
            IdentityCase {
                native_path_bytes: b"\\\\?\\C:\\repo\\\xed\xa0\x80",
                expected_identity_bytes: b"C:\\repo\\\xed\xa0\x80",
            },
            IdentityCase {
                native_path_bytes: br"C:\repo",
                expected_identity_bytes: br"C:\repo",
            },
            IdentityCase {
                native_path_bytes: br"\\server\share\repo",
                expected_identity_bytes: br"\\server\share\repo",
            },
            IdentityCase {
                native_path_bytes: b"\\\\?\\UNC\\server\\share\\\xed\xa0\x80",
                expected_identity_bytes: b"\\\\server\\share\\\xed\xa0\x80",
            },
            IdentityCase {
                native_path_bytes: b"/srv/\xff",
                expected_identity_bytes: b"/srv/\xff",
            },
            IdentityCase {
                native_path_bytes: b"/srv/project",
                expected_identity_bytes: b"/srv/project",
            },
        ];
        for case_row in case_rows {
            assert_eq!(
                workspace_hash_identity_bytes(case_row.native_path_bytes).as_ref(),
                case_row.expected_identity_bytes
            );
        }
    }

    #[test]
    fn workspace_hash_keys_ignore_windows_verbatim_namespaces() {
        struct NamespaceCase {
            verbatim_path: &'static str,
            plain_path: &'static str,
        }
        let case_rows = [
            NamespaceCase {
                verbatim_path: r"\\?\C:\repo",
                plain_path: r"C:\repo",
            },
            NamespaceCase {
                verbatim_path: r"\\?\UNC\server\share\repo",
                plain_path: r"\\server\share\repo",
            },
        ];
        for case_row in case_rows {
            assert_eq!(
                workspace_state_key(Path::new(case_row.verbatim_path)).expect("verbatim state key"),
                workspace_state_key(Path::new(case_row.plain_path)).expect("normal state key")
            );
        }
    }

    #[test]
    fn workspace_state_rejects_existing_regular_file() {
        let root = tempfile::tempdir().expect("fixture directory");
        let workspace_file = root.path().join("workspace-file");
        std::fs::write(&workspace_file, "not a directory").expect("workspace file");

        let error = workspace_state_dir(&workspace_file).expect_err("workspace file must fail closed");
        assert!(error.to_string().contains("workspace for persisted state is not a directory"));
    }

    #[test]
    fn workspace_state_preserves_missing_directory_resolution() {
        let root = tempfile::tempdir().expect("fixture directory");
        let missing_workspace = root.path().join("missing-workspace");

        let state_dir = workspace_state_dir(&missing_workspace).expect("missing workspace state path");
        assert!(state_dir.starts_with(crate::DiapasonPaths::resolve().expect("state paths").state_dir()));
        assert!(!missing_workspace.exists(), "resolving state must not create the workspace");
    }

    #[test]
    fn workspace_state_key_matches_independent_sha256_vector() {
        assert_eq!(
            workspace_state_key(Path::new("/srv/project")).expect("workspace state key"),
            "4f3f91f6b0b47e3bebd44da0bf3651775effa4a0bc22a35622a60cb34d4f962e"
        );
    }

    #[test]
    fn workspace_directory_names_reject_escape_and_upstream_storage() {
        for name in [
            "",
            ".",
            "..",
            "/tmp/escape",
            "nested/data",
            "nested\\data",
            ".vtcode",
            ".VTCODE",
        ] {
            assert!(validated_name(OsStr::new(name)).is_err(), "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn configured_workspace_data_rejects_symlink_escape() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), workspace.path().join(".custom-data")).unwrap();
        assert!(workspace_data_dir_for_name(workspace.path(), Path::new(".custom-data")).is_err());
    }

    #[test]
    fn configured_workspace_data_stays_inside_worktree() {
        let workspace = tempfile::tempdir().unwrap();
        let candidate = workspace_data_dir_for_name(workspace.path(), Path::new(".custom-data")).unwrap();
        assert_eq!(candidate, workspace.path().join(".custom-data"));
    }

    #[test]
    fn workspace_directory_name_accepts_custom_hidden_directory() {
        assert_eq!(validated_name(OsStr::new(".custom-data")).unwrap(), PathBuf::from(".custom-data"));
    }
}
