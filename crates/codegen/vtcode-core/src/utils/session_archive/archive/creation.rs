use super::super::{apply_session_retention_best_effort, resolve_sessions_dir};
use super::{
    SessionArchive, SessionArchiveMetadata, SessionListing, SessionSnapshot, generate_unique_archive_path,
    generate_unique_archive_path_for_label, session_archive_path_for_identifier,
};
use anyhow::Result;
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

fn session_identifier_from_archive_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow::anyhow!("failed to derive session identifier from archive path"))
}

fn reserve_new_session_archive_path(sessions_dir: &Path, session_identifier: &str) -> Result<PathBuf> {
    let path = session_archive_path_for_identifier(sessions_dir, session_identifier)?;
    if fs::symlink_metadata(&path).is_ok() {
        return Err(anyhow::anyhow!("Session archive identifier '{session_identifier}' already exists"));
    }

    Ok(path)
}

async fn resolve_sessions_dir_for_archive_writes() -> Result<PathBuf> {
    let sessions_dir = resolve_sessions_dir().await?;
    apply_session_retention_best_effort(&sessions_dir);
    Ok(sessions_dir)
}

/// Reserve a unique session archive identifier for the current process.
///
/// The returned identifier is the JSON file stem (without `.json`) and can be reused
/// to create an archive and pair external artifacts (for example debug logs).
pub async fn reserve_session_archive_identifier(
    workspace_label: &str,
    custom_suffix: Option<String>,
) -> Result<String> {
    let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
    let started_at = Utc::now();
    let path =
        generate_unique_archive_path_for_label(&sessions_dir, workspace_label, started_at, custom_suffix.as_deref());
    session_identifier_from_archive_path(&path)
}

impl SessionArchive {
    pub async fn new(metadata: SessionArchiveMetadata, custom_suffix: Option<String>) -> Result<Self> {
        let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
        let started_at = Utc::now();
        let path = generate_unique_archive_path(&sessions_dir, &metadata, started_at, custom_suffix.as_deref());

        Ok(Self::from_path(path, metadata, started_at))
    }

    /// Create a session archive using an explicitly reserved session identifier.
    pub async fn new_with_identifier(metadata: SessionArchiveMetadata, session_identifier: String) -> Result<Self> {
        let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
        let path = reserve_new_session_archive_path(&sessions_dir, &session_identifier)?;

        Ok(Self::from_path(path, metadata, Utc::now()))
    }

    /// Reopen an existing archive file so follow-up runs can overwrite the snapshot in place.
    pub fn resume_from_listing(listing: &SessionListing, metadata: SessionArchiveMetadata) -> Self {
        Self::from_path(listing.path.clone(), metadata, listing.snapshot.started_at)
    }

    /// Create a forked session from an existing session snapshot
    ///
    /// This creates a new session archive that inherits metadata from the source
    /// session but operates independently. The forked session will have a new
    /// archive file with a custom suffix if provided.
    ///
    /// # Arguments
    /// * `source_snapshot` - The snapshot of the session to fork from
    /// * `custom_suffix` - Optional custom suffix for the new session ID
    ///
    /// # Returns
    /// A new SessionArchive instance for the forked session
    pub async fn fork(source_snapshot: &SessionSnapshot, custom_suffix: Option<String>) -> Result<Self> {
        create_fork_archive(source_snapshot, custom_suffix, None).await
    }
}

async fn create_fork_archive(
    source_snapshot: &SessionSnapshot,
    custom_suffix: Option<String>,
    explicit_identifier: Option<String>,
) -> Result<SessionArchive> {
    let sessions_dir = resolve_sessions_dir_for_archive_writes().await?;
    let started_at = Utc::now();

    let forked_metadata = source_snapshot.metadata.fork_seed();

    let path = if let Some(session_identifier) = explicit_identifier {
        reserve_new_session_archive_path(&sessions_dir, &session_identifier)?
    } else {
        generate_unique_archive_path(&sessions_dir, &forked_metadata, started_at, custom_suffix.as_deref())
    };

    Ok(SessionArchive::from_path(path, forked_metadata, started_at))
}
