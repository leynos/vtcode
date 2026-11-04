use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use diapason_commons::DiapasonPaths;

use crate::config::{ConfigManager, PersistentMemoryConfig};

pub(crate) fn persistent_memory_base_dir(config: &PersistentMemoryConfig) -> Result<PathBuf> {
    if let Some(override_dir) = config.directory_override.as_deref() {
        if let Some(stripped) = override_dir.strip_prefix("~/") {
            return Ok(dirs::home_dir().context("Could not resolve home directory")?.join(stripped));
        }
        return Ok(PathBuf::from(override_dir));
    }
    DiapasonPaths::resolve()
        .map(|paths| paths.state_dir().to_path_buf())
        .context("Could not resolve Diapason state directory")
}

pub(crate) fn persistent_memory_project_name(workspace_root: &Path) -> String {
    ConfigManager::current_project_name(workspace_root)
        .or_else(|| workspace_root.file_name().and_then(|v| v.to_str()).map(|v| v.to_string()))
        .unwrap_or_else(|| "workspace".to_string())
}

#[cold]
pub(crate) fn sanitize_project_name(project_name: &str) -> String {
    let sanitized: String = project_name
        .chars()
        .map(|ch| match ch {
            '/' | '\\' | ':' => '_',
            other => other,
        })
        .collect();
    let trimmed = sanitized.trim();
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed.to_string()
    }
}
