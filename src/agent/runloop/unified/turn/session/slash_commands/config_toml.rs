use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use diapason_commons::DiapasonPaths;
use diapason_core::config::loader::ConfigManager;
use toml::Value as TomlValue;

pub(super) fn ensure_child_table<'a>(
    table: &'a mut toml::map::Map<String, TomlValue>,
    key: &str,
) -> &'a mut toml::map::Map<String, TomlValue> {
    let entry = table
        .entry(key.to_string())
        .or_insert_with(|| TomlValue::Table(Default::default()));
    if !entry.is_table() {
        *entry = TomlValue::Table(Default::default());
    }
    entry
        .as_table_mut()
        .expect("table entry should be a table after initialization")
}

pub(super) fn load_toml_value(path: &Path) -> Result<TomlValue> {
    if !path.exists() {
        return Ok(TomlValue::Table(Default::default()));
    }

    let content = fs::read_to_string(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if content.trim().is_empty() {
        return Ok(TomlValue::Table(Default::default()));
    }

    parse_toml_value(path, &content)
}

pub(super) fn load_private_toml_value(path: &Path) -> Result<TomlValue> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            anyhow::bail!("Refusing to read symlinked global config {}", path.display());
        }
        Ok(metadata) if !metadata.is_file() => {
            anyhow::bail!("Global config path is not a regular file: {}", path.display());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TomlValue::Table(Default::default()));
        }
        Err(error) => return Err(error).with_context(|| format!("Failed to inspect {}", path.display())),
    }

    let content = String::from_utf8(DiapasonPaths::read_file_no_follow(path)?)
        .with_context(|| format!("Failed to read {} as UTF-8", path.display()))?;
    if content.trim().is_empty() {
        return Ok(TomlValue::Table(Default::default()));
    }

    parse_toml_value(path, &content)
}

pub(super) fn save_toml_value(path: &Path, root: &TomlValue) -> Result<()> {
    let is_empty = root.as_table().is_some_and(|table| table.is_empty());
    if is_empty {
        if path.exists() {
            fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?;
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    fs::write(path, toml::to_string_pretty(root)?).with_context(|| format!("Failed to write {}", path.display()))
}

pub(super) fn save_private_toml_value(path: &Path, root: &TomlValue) -> Result<()> {
    let is_empty = root.as_table().is_some_and(|table| table.is_empty());
    if is_empty {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                anyhow::bail!("Refusing to remove symlinked global config {}", path.display());
            }
            Ok(metadata) if !metadata.is_file() => {
                anyhow::bail!("Global config path is not a regular file: {}", path.display());
            }
            Ok(_) => fs::remove_file(path).with_context(|| format!("Failed to remove {}", path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).with_context(|| format!("Failed to inspect {}", path.display())),
        }
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        DiapasonPaths::ensure_user_dir(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    }
    let content = toml::to_string_pretty(root)?;
    DiapasonPaths::write_private_file_atomic(path, content.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn parse_toml_value(path: &Path, content: &str) -> Result<TomlValue> {
    toml::from_str::<TomlValue>(content).with_context(|| format!("Failed to parse {}", path.display()))
}

pub(super) fn preferred_workspace_config_path(manager: &ConfigManager, workspace: &Path) -> PathBuf {
    manager.preferred_workspace_config_path(workspace)
}
