//! Session archive discovery, lookup, and search.

use super::{
    SessionListing, SessionSnapshot, VerifiedSessionArchiveIdentifier, is_session_file, resolve_sessions_dir,
    session_archive_path_for_identifier, validate_session_identifier,
};
use crate::llm::provider::{MessageContent, MessageRole};
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use vtcode_commons::fs::read_private_json_file;

impl SessionListing {
    pub fn identifier(&self) -> String {
        self.path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(|value| value.to_string())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    /// Return an identifier that is safe to advertise as an archive resume target.
    pub(super) fn verified_identifier(&self) -> Result<VerifiedSessionArchiveIdentifier> {
        let identifier = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .with_context(|| format!("session archive path has no valid identifier: {}", self.path.display()))?;
        validate_session_identifier(identifier)?;
        Ok(VerifiedSessionArchiveIdentifier(identifier.to_owned()))
    }

    pub fn first_prompt_preview(&self) -> Option<String> {
        self.preview_for_role(MessageRole::User)
    }

    pub fn first_reply_preview(&self) -> Option<String> {
        self.preview_for_role(MessageRole::Assistant)
    }

    fn preview_for_role(&self, role: MessageRole) -> Option<String> {
        self.snapshot.messages.iter().find_map(|message| {
            if message.role != role {
                return None;
            }

            let text_projection = message.content.as_text();
            if text_projection.trim().is_empty() {
                return None;
            }

            text_projection.lines().find_map(|line| {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(truncate_preview(trimmed, 80))
                }
            })
        })
    }
}

fn normalize_workspace_for_match(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
    };

    crate::utils::path::normalize_path(&absolute)
}

pub fn session_workspace_path(listing: &SessionListing) -> Option<PathBuf> {
    let raw = listing.snapshot.metadata.workspace_path.trim();
    if raw.is_empty() { None } else { Some(PathBuf::from(raw)) }
}

pub fn session_listing_matches_workspace(listing: &SessionListing, workspace: &Path) -> bool {
    let Some(session_workspace) = session_workspace_path(listing) else {
        return false;
    };

    normalize_workspace_for_match(&session_workspace) == normalize_workspace_for_match(workspace)
}

pub(in crate::utils) fn truncate_preview(input: &str, max_chars: usize) -> String {
    vtcode_commons::formatting::truncate_within(input, max_chars, "…")
}

pub async fn list_recent_sessions(limit: usize) -> Result<Vec<SessionListing>> {
    let sessions_dir = resolve_sessions_dir()
        .await
        .context("failed to resolve session directory for listing")?;

    let Some(session_paths) = scan_session_archive_paths(sessions_dir).await? else {
        return Ok(Vec::new());
    };

    let mut all_listings = load_session_listings(&session_paths).await?;
    all_listings.sort_by(|a, b| b.snapshot.ended_at.cmp(&a.snapshot.ended_at));
    if limit > 0 && all_listings.len() > limit {
        all_listings.truncate(limit);
    }

    Ok(all_listings)
}

async fn scan_session_archive_paths(sessions_dir: PathBuf) -> Result<Option<Vec<PathBuf>>> {
    // `std::fs::read_dir` is a blocking directory scan; run it off the async
    // executor and collect paths before spawning parallel read tasks. See `# Blocking`
    // docs in `src/agent/runloop/git.rs`.
    let scan_path = sessions_dir.clone();
    tokio::task::spawn_blocking(move || -> Result<Option<Vec<PathBuf>>> {
        let entries = match fs::read_dir(&sessions_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read session directory: {}", sessions_dir.display()));
            }
        };

        let mut paths = Vec::new();
        for entry in entries {
            let entry = entry.with_context(|| format!("failed to read session entry in {}", sessions_dir.display()))?;
            let path = entry.path();
            if is_session_file(&path) {
                paths.push(path);
            }
        }
        Ok(Some(paths))
    })
    .await
    .with_context(|| format!("session directory scan task failed for {}", scan_path.display()))?
}

async fn load_session_listings(session_paths: &[PathBuf]) -> Result<Vec<SessionListing>> {
    // Process session files in parallel for better performance with large archives
    // Batch processing to avoid overwhelming the system with too many concurrent tasks
    const BATCH_SIZE: usize = 10;
    let mut all_listings = Vec::new();

    for batch in session_paths.chunks(BATCH_SIZE) {
        all_listings.extend(load_session_listing_batch(batch).await?);
    }

    Ok(all_listings)
}

async fn load_session_listing_batch(batch: &[PathBuf]) -> Result<Vec<SessionListing>> {
    let tasks = batch
        .iter()
        .map(|path| {
            let path = path.clone();
            let task_path = path.clone();
            let task = tokio::task::spawn(async move {
                let snapshot = read_private_json_file::<SessionSnapshot>(&path)
                    .await
                    .with_context(|| format!("failed to load session archive {}", path.display()))?;
                Ok::<_, anyhow::Error>(SessionListing { path, snapshot })
            });
            (task_path, task)
        })
        .collect::<Vec<_>>();
    let mut listings = Vec::with_capacity(tasks.len());

    // Collect results from this batch.
    for (path, task) in tasks {
        let result = task
            .await
            .with_context(|| format!("failed to join session archive read task for {}", path.display()))?;
        match result {
            Ok(listing) => listings.push(listing),
            Err(error) => tracing::warn!(
                archive = %path.display(),
                %error,
                "Skipping unreadable session archive during listing"
            ),
        }
    }

    Ok(listings)
}

/// Find a session archive by its identifier (file stem) without needing to list all sessions.
pub async fn find_session_by_identifier(identifier: &str) -> Result<Option<SessionListing>> {
    let sessions_dir = match resolve_sessions_dir().await {
        Ok(dir) => dir,
        Err(_) => return Ok(None),
    };

    if !sessions_dir.exists() {
        return Ok(None);
    }

    let path = match session_archive_path_for_identifier(&sessions_dir, identifier) {
        Ok(path) => path,
        Err(_) => return Ok(None),
    };
    match tokio::fs::symlink_metadata(&path).await {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => return Ok(None),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to inspect session archive {}", path.display()));
        }
    }

    let snapshot: SessionSnapshot = read_private_json_file(&path).await?;
    Ok(Some(SessionListing { path, snapshot }))
}

/// Find a saved session by ID or name for TUI `/resume` command.
///
/// Searches both the session identifier (UUID or file stem) and matches
/// against recent sessions. Returns a descriptive error message when no
/// matching session is found.
pub async fn find_session_by_id_or_name(id_or_name: &str) -> Result<SessionListing> {
    // First try exact identifier match (UUID or file stem)
    if let Some(listing) = find_session_by_identifier(id_or_name).await? {
        return Ok(listing);
    }

    // Fall back to searching recent sessions by identifier substring
    let listings = list_recent_sessions(200).await?;
    let normalized_query = id_or_name.to_lowercase();

    for listing in &listings {
        if session_identifier_matches_query(listing, &normalized_query) {
            return Ok(listing.clone());
        }
    }

    Err(anyhow::anyhow!("No saved chat found matching '{id_or_name}'."))
}

fn session_identifier_matches_query(listing: &SessionListing, normalized_query: &str) -> bool {
    listing
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.to_lowercase().contains(normalized_query))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    pub session_id: String,
    pub session_path: PathBuf,
    pub timestamp: DateTime<Utc>,
    pub message_index: usize,
    pub role: MessageRole,
    pub content_snippet: String,
    pub score: f32, // Simple matching score (e.g., term frequency or just 1.0)
}

/// Search for a query string across recent sessions.
/// Returns a list of `SearchResult` sorted by relevance (or recency).
pub async fn search_sessions(query: &str, session_limit: usize, max_results: usize) -> Result<Vec<SearchResult>> {
    if query.trim().is_empty() || max_results == 0 {
        return Ok(Vec::new());
    }

    let listings = list_recent_sessions(session_limit).await?;
    let matcher = RegexBuilder::new(&regex::escape(query))
        .case_insensitive(true)
        .build()
        .context("failed to compile session search query")?;
    let mut results = Vec::new();

    for listing in listings {
        let remaining_results = max_results.saturating_sub(results.len());
        if remaining_results == 0 {
            break;
        }
        results.extend(search_listing_messages(&listing, &matcher, remaining_results));
    }

    Ok(results)
}

fn search_listing_messages(listing: &SessionListing, matcher: &regex::Regex, max_results: usize) -> Vec<SearchResult> {
    let mut results = Vec::new();

    for (idx, message) in listing.snapshot.messages.iter().enumerate() {
        let content = match &message.content {
            MessageContent::Text(text) => text.as_str(),
            MessageContent::Parts(_) => continue,
        };
        let Some(matched) = matcher.find(content) else {
            continue;
        };

        results.push(SearchResult {
            session_id: listing.identifier(),
            session_path: listing.path.clone(),
            timestamp: listing.snapshot.started_at,
            message_index: idx,
            role: message.role,
            content_snippet: search_result_snippet(content, matched.start(), matched.end()),
            score: 1.0,
        });

        if results.len() >= max_results {
            break;
        }
    }

    results
}

fn search_result_snippet(content: &str, match_start: usize, match_end: usize) -> String {
    const CONTEXT_BYTES: usize = 50;

    let start = floor_char_boundary(content, match_start.saturating_sub(CONTEXT_BYTES));
    let end = ceil_char_boundary(content, (match_end + CONTEXT_BYTES).min(content.len()));

    let mut snippet = String::new();
    if start > 0 {
        snippet.push_str("...");
    }
    snippet.push_str(&content[start..end].replace('\n', " "));
    if end < content.len() {
        snippet.push_str("...");
    }
    snippet
}

fn floor_char_boundary(content: &str, mut index: usize) -> usize {
    while index > 0 && !content.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(content: &str, mut index: usize) -> usize {
    while index < content.len() && !content.is_char_boundary(index) {
        index += 1;
    }
    index
}
