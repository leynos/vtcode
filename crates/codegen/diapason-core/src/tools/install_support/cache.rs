use anyhow::{Context, Result};
use diapason_commons::DiapasonPaths;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::path::{Path, PathBuf};

use super::time::unix_timestamp_now;

enum JsonCacheState<T> {
    Missing,
    Valid(T),
    Malformed(anyhow::Error),
}

fn read_json_cache_state<T: DeserializeOwned>(path: &Path, label: &str) -> Result<JsonCacheState<T>> {
    let bytes = match DiapasonPaths::read_file_no_follow(path) {
        Ok(bytes) => bytes,
        Err(error) if is_not_found(&error) => return Ok(JsonCacheState::Missing),
        Err(error) => return Err(error).with_context(|| format!("Failed to read {} at {}", label, path.display())),
    };

    let content = match String::from_utf8(bytes) {
        Ok(content) => content,
        Err(error) => {
            return Ok(JsonCacheState::Malformed(anyhow::anyhow!(
                "Failed to read {} at {} as UTF-8: {error}",
                label,
                path.display()
            )));
        }
    };
    match serde_json::from_str(&content) {
        Ok(value) => Ok(JsonCacheState::Valid(value)),
        Err(error) => Ok(JsonCacheState::Malformed(anyhow::anyhow!(
            "Failed to parse {} at {}: {error}",
            label,
            path.display()
        ))),
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::NotFound)
    })
}

/// Load a cache from its canonical location, falling back to legacy paths
/// when the canonical file is missing or malformed. A successfully loaded
/// legacy cache is republished best-effort so subsequent processes use the
/// canonical location without losing the fallback if the write is unavailable.
pub(crate) fn load_json_cache_with_legacy_fallback<T>(
    canonical_path: &Path,
    legacy_paths: &[PathBuf],
    label: &str,
) -> Result<T>
where
    T: DeserializeOwned + Serialize,
{
    let canonical_state = read_json_cache_state(canonical_path, label)?;
    let canonical_missing = matches!(canonical_state, JsonCacheState::Missing);
    let canonical_error = match canonical_state {
        JsonCacheState::Valid(value) => return Ok(value),
        JsonCacheState::Missing => None,
        JsonCacheState::Malformed(error) => Some(error),
    };

    let mut last_error = canonical_error;
    for legacy_path in legacy_paths {
        if legacy_path == canonical_path {
            continue;
        }
        match read_json_cache_state(legacy_path, label) {
            Ok(JsonCacheState::Valid(value)) => {
                if canonical_missing {
                    if let Some(parent) = canonical_path.parent() {
                        let _ = save_json_cache_if_absent(parent, canonical_path, &value, label);
                    }
                }
                return Ok(value);
            }
            Ok(JsonCacheState::Missing) => {}
            Ok(JsonCacheState::Malformed(error)) => last_error = Some(error),
            Err(error) => last_error = Some(error),
        }
    }

    last_error.map_or_else(|| Err(anyhow::anyhow!("{} does not exist at {}", label, canonical_path.display())), Err)
}

pub(crate) fn save_json_cache<T: Serialize>(state_dir: &Path, path: &Path, value: &T, label: &str) -> Result<()> {
    DiapasonPaths::ensure_user_dir(state_dir).with_context(|| format!("Failed to create {}", state_dir.display()))?;
    let bytes = serde_json::to_vec(value).with_context(|| format!("Failed to serialize {label}"))?;
    DiapasonPaths::with_private_file_lock(path, || DiapasonPaths::write_private_file_atomic(path, &bytes))
        .with_context(|| format!("Failed to write {} at {}", label, path.display()))?;
    Ok(())
}

pub(crate) fn save_json_cache_if_absent<T: Serialize>(
    state_dir: &Path,
    path: &Path,
    value: &T,
    label: &str,
) -> Result<bool> {
    DiapasonPaths::ensure_user_dir(state_dir).with_context(|| format!("Failed to create {}", state_dir.display()))?;
    let bytes = serde_json::to_vec(value).with_context(|| format!("Failed to serialize {label}"))?;
    DiapasonPaths::with_private_file_lock(path, || DiapasonPaths::write_private_file_atomic_if_absent(path, &bytes))
        .with_context(|| format!("Failed to write {} at {}", label, path.display()))
}

pub(crate) fn cache_is_stale(last_attempt: u64, stale_after_secs: u64) -> bool {
    unix_timestamp_now().saturating_sub(last_attempt) > stale_after_secs
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn malformed_canonical_cache_recovers_legacy_without_replacing_it() {
        let temp_dir = tempdir().expect("temp dir");
        let canonical_path = temp_dir.path().join("canonical/cache.json");
        let legacy_path = temp_dir.path().join("legacy/cache.json");
        std::fs::create_dir_all(legacy_path.parent().expect("legacy parent")).expect("legacy directory");
        std::fs::create_dir_all(canonical_path.parent().expect("canonical parent")).expect("canonical directory");
        std::fs::write(&canonical_path, b"not json").expect("malformed canonical cache");
        let legacy = serde_json::json!({"status": "legacy"});
        std::fs::write(&legacy_path, serde_json::to_vec(&legacy).expect("serialize legacy cache"))
            .expect("legacy cache");

        let loaded: serde_json::Value =
            load_json_cache_with_legacy_fallback(&canonical_path, std::slice::from_ref(&legacy_path), "test cache")
                .expect("recover legacy cache");

        assert_eq!(loaded, legacy);
        assert_eq!(std::fs::read(&canonical_path).expect("read canonical cache"), b"not json");
    }
}
