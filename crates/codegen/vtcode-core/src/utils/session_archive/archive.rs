//! Session archive creation and durable snapshot writes.

use super::{
    SessionArchiveMetadata, SessionContinuationMetadata, SessionListing, SessionMessage, SessionProgress,
    SessionProgressArgs, SessionProgressPersistenceStatus, SessionSnapshot, SnapshotTurnDiagnostics,
    VerifiedSessionArchiveIdentifier, apply_session_retention_best_effort, clean_transcript_lines,
    find_session_by_identifier, history_persistence_enabled, normalize_distinct_tools_for_summary,
    prepare_snapshot_for_write, progress_transcript_from_recent_messages, resolve_sessions_dir,
};
use crate::config::constants::defaults;
use crate::telemetry::perf::PerfSpan;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vtcode_commons::VtCodePaths;
use vtcode_commons::fs::write_private_json_file;

const SESSION_FILE_PREFIX: &str = "session";
const SESSION_FILE_EXTENSION: &str = "json";

struct SessionSnapshotContent {
    total_messages: usize,
    distinct_tools: Vec<String>,
    transcript: Vec<String>,
    messages: Vec<SessionMessage>,
    progress: Option<SessionProgress>,
}

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

fn session_identifier_from_archive_path(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|value| value.to_string())
        .ok_or_else(|| anyhow::anyhow!("failed to derive session identifier from archive path"))
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

fn write_private_json_file_sync<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let contents =
        serde_json::to_vec_pretty(value).with_context(|| format!("failed to serialize {}", path.display()))?;
    VtCodePaths::write_private_file_atomic(path, &contents)
        .with_context(|| format!("failed to write private session archive {}", path.display()))
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

    fn build_snapshot(&self, content: SessionSnapshotContent) -> SessionSnapshot {
        use crate::utils::error_log_collector::drain_error_logs;

        SessionSnapshot {
            metadata: self.metadata.clone(),
            started_at: self.started_at,
            ended_at: Utc::now(),
            total_messages: content.total_messages,
            distinct_tools: content.distinct_tools,
            transcript: content.transcript,
            messages: content.messages,
            progress: content.progress.map(Box::new),
            error_logs: drain_error_logs(),
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

    fn build_progress_snapshot(&self, args: SessionProgressArgs) -> SessionSnapshot {
        let SessionProgressArgs {
            total_messages,
            distinct_tools,
            messages,
            recent_messages,
            turn_number,
            token_usage,
            max_context_tokens,
            loaded_skills,
            turn_diagnostics,
        } = args;

        let transcript = progress_transcript_from_recent_messages(&recent_messages);
        let distinct_tools = normalize_distinct_tools_for_summary(&distinct_tools);
        let tool_summaries = distinct_tools.clone();

        self.build_snapshot(SessionSnapshotContent {
            total_messages,
            distinct_tools,
            transcript,
            messages,
            progress: Some(SessionProgress {
                turn_number,
                recent_messages,
                tool_summaries,
                token_usage,
                max_context_tokens,
                loaded_skills: loaded_skills.unwrap_or_default(),
                turn_diagnostics,
            }),
        })
    }

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

    pub fn finalize(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
    ) -> Result<PathBuf> {
        self.finalize_with_diagnostics(transcript, total_messages, distinct_tools, messages, None)
    }

    /// Finalize the archive while retaining lightweight diagnostics for the
    /// last turn. The final snapshot still omits the heavy progress history.
    pub fn finalize_with_diagnostics(
        &self,
        transcript: Vec<String>,
        total_messages: usize,
        distinct_tools: Vec<String>,
        messages: Vec<SessionMessage>,
        turn_diagnostics: Option<SnapshotTurnDiagnostics>,
    ) -> Result<PathBuf> {
        let snapshot = self.build_snapshot(SessionSnapshotContent {
            total_messages,
            distinct_tools: normalize_distinct_tools_for_summary(&distinct_tools),
            transcript: clean_transcript_lines(&transcript),
            messages,
            progress: turn_diagnostics.map(|turn_diagnostics| SessionProgress {
                turn_diagnostics: Some(turn_diagnostics),
                ..SessionProgress::default()
            }),
        });

        let path = self.write_snapshot(snapshot)?;
        if let Some(parent) = path.parent() {
            apply_session_retention_best_effort(parent);
        }
        Ok(path)
    }

    pub fn persist_progress(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", "sync");

        let snapshot = self.build_progress_snapshot(args);

        self.write_snapshot(snapshot)
    }

    /// Persist progress asynchronously and report whether this call wrote a
    /// new durable snapshot.
    ///
    /// A throttled checkpoint still returns the archive path for diagnostics,
    /// but it must not be treated as durable by callers that acknowledge
    /// state only after history has been written.
    pub async fn persist_progress_async_with_status(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, false).await
    }

    async fn persist_progress_async_with_status_inner(
        &self,
        args: SessionProgressArgs,
        bypass_throttle: bool,
    ) -> Result<SessionProgressPersistenceStatus> {
        let mut perf = PerfSpan::new("vtcode.perf.session_progress_write_ms");
        perf.tag("mode", if bypass_throttle { "async_forced" } else { "async" });

        if !history_persistence_enabled() {
            return Ok(SessionProgressPersistenceStatus::Disabled(self.path.clone()));
        }
        if !bypass_throttle && !self.should_persist_progress(args.turn_number)? {
            return Ok(SessionProgressPersistenceStatus::Throttled(self.path.clone()));
        }

        let snapshot = self.build_progress_snapshot(args);
        let path = self.write_snapshot_async(snapshot).await?;
        Ok(SessionProgressPersistenceStatus::Persisted(path))
    }

    /// Persist progress immediately, bypassing the normal progress throttle.
    ///
    /// This is intended for terminal handoff checkpoints where the latest
    /// conversation must be durable before an external artifact is written.
    pub async fn persist_progress_async_with_status_forced(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_inner(args, true).await
    }

    /// Persist a durable checkpoint without applying the progress throttle.
    ///
    /// Callers use this at state boundaries where losing the most recent
    /// message would make a resumed session inconsistent. History persistence
    /// and payload-size policy still apply.
    pub async fn persist_checkpoint_async(
        &self,
        args: SessionProgressArgs,
    ) -> Result<SessionProgressPersistenceStatus> {
        self.persist_progress_async_with_status_forced(args).await
    }

    pub async fn persist_progress_async(&self, args: SessionProgressArgs) -> Result<PathBuf> {
        Ok(self.persist_progress_async_with_status(args).await?.path().to_path_buf())
    }

    fn write_snapshot(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file_sync(&self.path, &snapshot)?;
        Ok(self.path.clone())
    }

    async fn write_snapshot_async(&self, snapshot: SessionSnapshot) -> Result<PathBuf> {
        let Some(snapshot) = prepare_snapshot_for_write(snapshot)? else {
            return Ok(self.path.clone());
        };

        write_private_json_file(&self.path, &snapshot).await?;
        Ok(self.path.clone())
    }

    fn should_persist_progress(&self, turn_number: usize) -> Result<bool> {
        let min_interval = Duration::from_millis(defaults::DEFAULT_SESSION_PROGRESS_MIN_INTERVAL_MS);
        let min_turns = defaults::DEFAULT_SESSION_PROGRESS_MIN_TURN_DELTA;

        let mut throttle = self
            .progress_throttle
            .lock()
            .map_err(|err| anyhow::anyhow!("session progress throttle lock poisoned: {err}"))
            .context("Failed to evaluate session progress persistence throttle")?;
        if turn_number <= throttle.last_turn {
            return Ok(false);
        }
        if throttle.last_written.elapsed() < min_interval && turn_number.saturating_sub(throttle.last_turn) < min_turns
        {
            return Ok(false);
        }

        throttle.last_written = Instant::now();
        throttle.last_turn = turn_number;
        Ok(true)
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
