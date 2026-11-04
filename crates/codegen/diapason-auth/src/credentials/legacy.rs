//! Legacy plaintext auth.json migration.
//!
//! In earlier versions API keys were stored in a plaintext `auth.json` file.
//! These functions detect and read such entries so callers can migrate them
//! into the current encrypted storage format without deleting the rollback
//! source.

use anyhow::{Context, Result, anyhow};
use std::fs;

use crate::storage_paths::{legacy_auth_storage_paths, read_legacy_compatible_file};

#[derive(Debug, serde::Deserialize)]
pub(crate) struct LegacyAuthFile {
    mode: String,
    provider: String,
    pub(crate) api_key: String,
}

#[derive(Debug)]
pub(crate) struct LegacyAuthEntry {
    pub(crate) credentials: LegacyAuthFile,
    pub(crate) path: std::path::PathBuf,
}

/// Find and return a legacy auth.json entry for `provider`, if one exists.
pub(crate) fn load_for_provider(provider: &str) -> Result<Option<LegacyAuthEntry>> {
    for path in legacy_auth_storage_paths()? {
        let Some(data) = read_legacy_compatible_file(&path)? else {
            continue;
        };

        let legacy: LegacyAuthFile = serde_json::from_slice(&data)
            .with_context(|| format!("failed to parse legacy auth file {}", path.display()))?;
        let matches_provider = legacy.provider.eq_ignore_ascii_case(provider);
        let stores_api_key = legacy.mode.eq_ignore_ascii_case("api_key");
        let has_key = !legacy.api_key.trim().is_empty();

        if matches_provider && stores_api_key && has_key {
            return Ok(Some(LegacyAuthEntry { credentials: legacy, path }));
        }
    }
    Ok(None)
}

/// Delete matching legacy auth files after an explicit user request.
///
/// Automatic migration deliberately does not call this function: the legacy
/// directory is retained as a rollback-safe backup.
pub(crate) fn clear_for_provider(provider: &str) -> Result<()> {
    for path in legacy_auth_storage_paths()? {
        let Some(data) = read_legacy_compatible_file(&path)? else {
            continue;
        };

        let Ok(legacy) = serde_json::from_slice::<LegacyAuthFile>(&data) else {
            continue;
        };

        if legacy.mode.eq_ignore_ascii_case("api_key") && legacy.provider.eq_ignore_ascii_case(provider) {
            delete_file(&path)?;
        }
    }
    Ok(())
}

/// Remove the legacy auth.json file if it exists (ignoring absent).
pub(crate) fn delete_file(path: &std::path::Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(anyhow!("failed to delete legacy auth file: {err}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diapason_commons::env_lock;
    use serial_test::serial;
    use tempfile::TempDir;

    #[test]
    #[serial]
    fn ignores_nested_legacy_auth_file_from_explicit_diapason_home() {
        let env = env_lock::lock();
        let config = TempDir::new().expect("create config root");
        let legacy = TempDir::new().expect("create legacy root");
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_home = std::env::var_os("DIAPASON_HOME");
        env.set_var("XDG_CONFIG_HOME", config.path());
        env.set_var("DIAPASON_HOME", legacy.path());

        let nested_auth = legacy.path().join("auth/auth.json");
        fs::create_dir_all(nested_auth.parent().expect("nested auth parent")).expect("create legacy auth dir");
        fs::write(&nested_auth, r#"{"mode":"api_key","provider":"openai","api_key":"legacy-key"}"#)
            .expect("write nested legacy auth file");

        assert!(load_for_provider("openai").expect("inspect canonical auth only").is_none());

        env.restore_var("XDG_CONFIG_HOME", previous_config);
        env.restore_var("DIAPASON_HOME", previous_home);
    }
}
