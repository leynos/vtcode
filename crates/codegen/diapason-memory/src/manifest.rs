use std::fs;
use std::path::Path;

use crate::SessionManifest;
use crate::TurnIndex;
use crate::error::SessionStoreError;

/// Manifest persistence helpers.
///
/// Separated from `event_log` so the hot append path does not carry
/// serialization concerns, and so `open` can cheaply probe the manifest
/// before deciding whether to run the O(n) scan.
pub struct ManifestStore {
    session_dir: std::path::PathBuf,
}

impl ManifestStore {
    /// Create a new manifest store for the given session directory.
    pub(crate) fn new(session_dir: std::path::PathBuf) -> Self {
        Self { session_dir }
    }

    /// Path to `manifest.json` inside the session directory.
    fn manifest_path(&self) -> std::path::PathBuf {
        self.session_dir.join("manifest.json")
    }

    /// Path to `index/turns.json` inside the session directory.
    fn turns_path(&self) -> std::path::PathBuf {
        self.session_dir.join("index").join("turns.json")
    }

    /// Load the manifest if it exists and is parseable.
    ///
    /// Returns `Ok(None)` when the file is missing (fresh session) or
    /// malformed, rather than erroring — the caller can fall back to scanning
    /// the event log.
    pub(crate) fn load_manifest(&self) -> Result<Option<SessionManifest>, SessionStoreError> {
        let path = self.manifest_path();
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }

    /// Load the turn index if it exists and is parseable.
    ///
    /// Returns `Ok(None)` when the file is missing or malformed.
    pub(crate) fn load_turn_index(&self) -> Result<Option<TurnIndex>, SessionStoreError> {
        let path = self.turns_path();
        let Some(bytes) = read_optional_private_file(&path)? else {
            return Ok(None);
        };
        Ok(serde_json::from_slice(&bytes).ok())
    }
    /// Atomically write the manifest. Parent directories must already exist.
    pub(crate) fn write_manifest(&self, manifest: &SessionManifest) -> Result<(), SessionStoreError> {
        let path = self.manifest_path();
        let bytes = serde_json::to_vec(manifest)?;
        diapason_commons::DiapasonPaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path, std::io::Error::other(error)))
    }

    /// Atomically write the turn index. Parent directories must already exist.
    pub(crate) fn write_turn_index(&self, index: &TurnIndex) -> Result<(), SessionStoreError> {
        let path = self.turns_path();
        let bytes = serde_json::to_vec(index)?;
        diapason_commons::DiapasonPaths::write_private_file_atomic(&path, &bytes)
            .map_err(|error| SessionStoreError::io(path, std::io::Error::other(error)))
    }
}

fn read_optional_private_file(path: &Path) -> Result<Option<Vec<u8>>, SessionStoreError> {
    match fs::symlink_metadata(path) {
        Ok(_) => diapason_commons::DiapasonPaths::read_file_no_follow(path)
            .map(Some)
            .map_err(|error| SessionStoreError::io(path.to_path_buf(), std::io::Error::other(error))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(SessionStoreError::io(path.to_path_buf(), error)),
    }
}
