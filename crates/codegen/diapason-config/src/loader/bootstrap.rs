use diapason_commons::DiapasonPaths;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::defaults::ConfigDefaultsProvider;

const DEFAULT_GITIGNORE_FILE_NAME: &str = ".diapasongitignore";

/// Determine where configuration and gitignore files should be created when
/// bootstrapping a workspace.
pub(crate) fn determine_bootstrap_targets(
    workspace: &Path,
    use_home_dir: bool,
    config_file_name: &str,
    defaults_provider: &dyn ConfigDefaultsProvider,
) -> Result<(PathBuf, PathBuf)> {
    if use_home_dir {
        if let Some(home_config_path) = select_home_config_path(defaults_provider, config_file_name)? {
            let gitignore_path = gitignore_path_for(&home_config_path);
            return Ok((home_config_path, gitignore_path));
        }
    }

    let config_path = workspace.join(config_file_name);
    let gitignore_path = workspace.join(DEFAULT_GITIGNORE_FILE_NAME);
    Ok((config_path, gitignore_path))
}

/// Returns the preferred gitignore path for a given configuration file.
fn gitignore_path_for(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join(DEFAULT_GITIGNORE_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GITIGNORE_FILE_NAME))
}

/// Ensures the parent directory for the provided path exists, creating it if
/// necessary.
pub(crate) fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Failed to create directory: {}", parent.display()))?;
    }

    Ok(())
}

/// Ensures a canonical user-level configuration parent exists privately.
pub(crate) fn ensure_private_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        let _ = DiapasonPaths::ensure_user_dir(parent)
            .with_context(|| format!("Failed to create private directory: {}", parent.display()))?;
    }

    Ok(())
}

/// Selects the canonical user configuration path from the defaults provider.
fn select_home_config_path(
    defaults_provider: &dyn ConfigDefaultsProvider,
    config_file_name: &str,
) -> Result<Option<PathBuf>> {
    defaults_provider
        .canonical_user_config_path(config_file_name)
        .context("Failed to resolve canonical user configuration path")
}
