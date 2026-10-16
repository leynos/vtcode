//! Session archive creation and durable snapshot writes.

mod creation;
mod persistence;

use super::{
    SessionArchiveMetadata, SessionContinuationMetadata, SessionListing, SessionSnapshot,
    VerifiedSessionArchiveIdentifier, find_session_by_identifier,
};
use crate::config::constants::defaults;
use anyhow::Result;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SESSION_FILE_PREFIX: &str = "session";
const SESSION_FILE_EXTENSION: &str = "json";

pub(in crate::utils) fn generate_unique_archive_path(
    sessions_dir: &Path,
    metadata: &SessionArchiveMetadata,
    started_at: DateTime<Utc>,
    custom_suffix: Option<&str>,
) -> PathBuf {
    generate_unique_archive_path_for_label(sessions_dir, &metadata.workspace_label, started_at, custom_suffix)
}

fn generate_unique_archive_path_for_label(
    sessions_dir: &Path,
    workspace_label: &str,
    started_at: DateTime<Utc>,
    custom_suffix: Option<&str>,
) -> PathBuf {
    if custom_suffix.is_some() {
        return sessions_dir.join(archive_file_name_for_label(workspace_label, started_at, custom_suffix, None));
    }

    let mut attempt = 0u32;
    loop {
        let candidate =
            sessions_dir.join(archive_file_name_for_label(workspace_label, started_at, None, Some(attempt)));
        if !candidate.exists() {
            return candidate;
        }
        attempt = attempt.wrapping_add(1);
    }
}

fn archive_file_name_for_label(
    workspace_label: &str,
    started_at: DateTime<Utc>,
    custom_suffix: Option<&str>,
    attempt: Option<u32>,
) -> String {
    let sanitized_label = sanitize_component(workspace_label);
    let timestamp = started_at.format("%Y%m%dT%H%M%SZ").to_string();

    if let Some(suffix) = custom_suffix {
        return format!(
            "{}-{}-{}-{}.{}",
            SESSION_FILE_PREFIX,
            sanitized_label,
            timestamp,
            sanitize_component(suffix),
            SESSION_FILE_EXTENSION
        );
    }

    let micros = started_at.timestamp_subsec_micros();
    let pid = process::id();
    let attempt_suffix = match attempt.unwrap_or_default() {
        0 => String::new(),
        value => format!("-{value:02}"),
    };
    format!(
        "{SESSION_FILE_PREFIX}-{sanitized_label}-{timestamp}_{micros:06}-{pid:05}{attempt_suffix}.{SESSION_FILE_EXTENSION}"
    )
}

pub fn generate_session_archive_identifier(workspace_label: &str, custom_suffix: Option<String>) -> String {
    let file_name = archive_file_name_for_label(workspace_label, Utc::now(), custom_suffix.as_deref(), Some(0));
    Path::new(&file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("session-{}-{}", sanitize_component(workspace_label), process::id()))
}

fn is_valid_session_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

pub(in crate::utils) fn validate_session_identifier(session_identifier: &str) -> Result<()> {
    if is_valid_session_identifier(session_identifier) {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "Invalid session identifier '{session_identifier}': only ASCII letters, digits, '-' and '_' are allowed"
    ))
}

pub(in crate::utils) fn session_archive_path_for_identifier(
    sessions_dir: &Path,
    session_identifier: &str,
) -> Result<PathBuf> {
    validate_session_identifier(session_identifier)?;
    Ok(sessions_dir.join(format!("{session_identifier}.{SESSION_FILE_EXTENSION}")))
}

fn sanitize_component(value: &str) -> String {
    let mut normalized = String::new();
    let mut last_was_separator = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if matches!(ch, '-' | '_') {
            if !last_was_separator {
                normalized.push(ch);
                last_was_separator = true;
            }
        } else if !last_was_separator {
            normalized.push('-');
            last_was_separator = true;
        }
    }

    let trimmed = normalized.trim_matches(|c| c == '-' || c == '_');
    if trimmed.is_empty() {
        "workspace".to_owned()
    } else {
        trimmed.to_owned()
    }
}

pub(in crate::utils) fn is_session_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some(ext) if ext.eq_ignore_ascii_case(SESSION_FILE_EXTENSION) || ext.eq_ignore_ascii_case("jsonl")
    )
}

#[derive(Debug, Clone)]
pub struct SessionArchive {
    path: PathBuf,
    metadata: SessionArchiveMetadata,
    started_at: DateTime<Utc>,
    progress_throttle: Arc<Mutex<ProgressThrottle>>,
}

#[derive(Debug)]
struct ProgressThrottle {
    last_written: Instant,
    last_turn: usize,
}

impl ProgressThrottle {
    fn new() -> Self {
        let min_interval = Duration::from_millis(defaults::DEFAULT_SESSION_PROGRESS_MIN_INTERVAL_MS);
        let last_written = Instant::now().checked_sub(min_interval).unwrap_or_else(Instant::now);
        Self { last_written, last_turn: 0 }
    }
}

impl SessionArchive {
    fn from_path(path: PathBuf, metadata: SessionArchiveMetadata, started_at: DateTime<Utc>) -> Self {
        Self {
            path,
            metadata,
            started_at,
            progress_throttle: Arc::new(Mutex::new(ProgressThrottle::new())),
        }
    }

    /// Records the active primary agent (session "mode") so a later resume can
    /// restore it. Called once the active agent is selected, after the archive
    /// and thread have been started.
    pub fn set_primary_agent(&mut self, primary_agent: impl Into<String>) {
        self.metadata.primary_agent = Some(primary_agent.into());
    }

    pub fn metadata(&self) -> &SessionArchiveMetadata {
        &self.metadata
    }

    /// Update loaded skills in the archive metadata
    pub fn set_loaded_skills(&mut self, skills: Vec<String>) {
        self.metadata.loaded_skills = skills;
    }

    /// Replace the metadata captured in subsequent snapshots.
    pub fn replace_metadata(&mut self, metadata: SessionArchiveMetadata) {
        self.metadata = metadata;
    }

    /// Update continuation metadata in the archive metadata.
    pub fn set_continuation_metadata(&mut self, continuation_metadata: Option<SessionContinuationMetadata>) {
        self.metadata.continuation_metadata = continuation_metadata;
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verify that a persisted snapshot resolves back to this archive and
    /// return its typed resume identifier.
    pub async fn verify_persisted_resume_identifier(
        &self,
        persisted_path: &Path,
    ) -> Result<Option<VerifiedSessionArchiveIdentifier>> {
        if self.path != persisted_path {
            return Ok(None);
        }

        let Some(identifier) = persisted_path.file_stem().and_then(|stem| stem.to_str()) else {
            return Ok(None);
        };
        let Some(listing) = find_session_by_identifier(identifier).await? else {
            return Ok(None);
        };
        if listing.path != self.path {
            return Ok(None);
        }

        Ok(Some(listing.verified_identifier()?))
    }
}

pub use creation::reserve_session_archive_identifier;
