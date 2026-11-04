use anyhow::{Context, Result, anyhow, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{Duration, sleep};

use crate::config::PersistentMemoryConfig;
use crate::config::loader::DiapasonConfig;
use crate::config::types::AgentConfig as RuntimeAgentConfig;
use crate::llm::factory::infer_provider_from_model;
use crate::llm::provider::{LLMProvider, LLMRequest, Message, MessageRole};
use crate::llm::{
    LightweightFeature, collect_single_response, create_provider_for_model_route, resolve_lightweight_route,
};
use diapason_commons::DiapasonPaths;

mod fact_extraction;
mod llm_ops;
mod lock;
mod paths;
mod reader;
mod rendering;

pub use fact_extraction::{
    dedup_latest_facts, maybe_extract_tool_fact, maybe_extract_user_fact, normalize_whitespace, truncate_for_fact,
};
use llm_ops::{classify_facts_strict, plan_memory_operation, summarize_memory};
use lock::MemoryLock;
use paths::{persistent_memory_base_dir, persistent_memory_project_name, sanitize_project_name};
use reader::{
    MemoryNoteSummary, classify_fact, collect_all_memory_matches, collect_cleanup_candidates, collect_memory_matches,
    count_pending_rollout_summaries, count_pending_rollout_summaries_async, decode_topic_source, encode_topic_source,
    list_pending_rollout_files_async, list_rollout_markdown_files, list_rollout_markdown_files_async,
    normalize_memory_query, parse_fact_line, parse_topic_file, read_note_summaries, read_rollout_records,
    read_topic_records,
};
use rendering::{
    render_memory_index, render_memory_summary, render_memory_summary_bullets, render_rollout_summary,
    render_topic_file, unique_rollout_id,
};

// Re-exported at crate-visible-but-restricted scope solely so that
// `persistent_memory_tests` (a descendant module of `persistent_memory`, see
// `#[cfg(test)] mod persistent_memory_tests;` below) can reach these
// otherwise-internal submodule items through its `use super::*;`. They are
// not referenced by any non-test code in this module, so the imports are
// gated behind `#[cfg(test)]` to avoid unused-import warnings in normal
// builds.
#[cfg(test)]
use llm_ops::{
    MemoryModelRoute, MemoryPhase, classify_facts_with_provider, plan_memory_operation_with_provider,
    resolve_memory_model_routes, summarize_memory_with_provider,
};
#[cfg(test)]
use lock::{LOCK_STALE_AFTER_SECS, lock_age};

pub const MEMORY_FILENAME: &str = "MEMORY.md";
pub const MEMORY_SUMMARY_FILENAME: &str = "memory_summary.md";
pub const ROLLOUT_SUMMARIES_DIRNAME: &str = "rollout_summaries";
pub const NOTES_DIRNAME: &str = "notes";

const MEMORY_LOCK_FILENAME: &str = ".memory.lock";
const PREFERENCES_FILENAME: &str = "preferences.md";
const REPOSITORY_FACTS_FILENAME: &str = "repository-facts.md";
const DEFAULT_FACT_LIMIT: usize = 24;
const MEMORY_HIGHLIGHT_LIMIT: usize = 10;
const TOPIC_FACT_LIMIT: usize = 32;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GroundedFactRecord {
    pub fact: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersistentMemoryStatus {
    pub enabled: bool,
    pub auto_write: bool,
    pub directory: PathBuf,
    pub summary_file: PathBuf,
    pub memory_file: PathBuf,
    pub preferences_file: PathBuf,
    pub repository_facts_file: PathBuf,
    pub notes_dir: PathBuf,
    pub rollout_summaries_dir: PathBuf,
    pub summary_exists: bool,
    pub registry_exists: bool,
    pub pending_rollout_summaries: usize,
    pub cleanup_status: MemoryCleanupStatus,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistentMemoryExcerpt {
    pub status: PersistentMemoryStatus,
    pub contents: String,
    pub truncated: bool,
    pub bytes_read: usize,
    pub lines_read: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistentMemoryWriteReport {
    pub directory: PathBuf,
    pub summary_file: PathBuf,
    pub memory_file: PathBuf,
    pub rollout_summary_file: Option<PathBuf>,
    pub created_files: Vec<PathBuf>,
    pub added_facts: usize,
    pub pending_rollout_summaries: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersistentMemoryMatch {
    pub source: String,
    pub fact: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PersistentMemoryForgetReport {
    pub directory: PathBuf,
    pub summary_file: PathBuf,
    pub memory_file: PathBuf,
    pub removed_facts: usize,
    pub pending_rollout_summaries: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MemoryCleanupStatus {
    pub needed: bool,
    pub suspicious_facts: usize,
    pub suspicious_summary_lines: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersistentMemoryCleanupReport {
    pub directory: PathBuf,
    pub summary_file: PathBuf,
    pub memory_file: PathBuf,
    pub rewritten_facts: usize,
    pub removed_rollout_files: usize,
}

pub fn extract_memory_highlights(contents: &str, limit: usize) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let mut highlights = Vec::with_capacity(limit);
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let normalized = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
            .or_else(|| trimmed.strip_prefix("+ "))
            .unwrap_or(trimmed);
        if normalized.is_empty() || highlights.iter().any(|e| e == normalized) {
            continue;
        }
        highlights.push(normalized.to_string());
        if highlights.len() >= limit {
            break;
        }
    }
    highlights
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryOpKind {
    Remember,
    Forget,
    AskMissing,
    Noop,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOpCandidate {
    pub id: usize,
    pub source: String,
    pub fact: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryPlannedTopic {
    Preferences,
    RepositoryFacts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryPlannedFact {
    pub topic: MemoryPlannedTopic,
    pub fact: String,
    #[serde(default)]
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryMissingField {
    pub field: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryOpPlan {
    pub kind: MemoryOpKind,
    #[serde(default)]
    pub facts: Vec<MemoryPlannedFact>,
    #[serde(default)]
    pub selected_ids: Vec<usize>,
    #[serde(default)]
    pub missing: Option<MemoryMissingField>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum MemoryTopic {
    Preferences = 0,
    RepositoryFacts = 1,
}

impl MemoryTopic {
    fn title(self) -> &'static str {
        ["Preferences", "Repository Facts"][self as usize]
    }

    fn description(self) -> &'static str {
        [
            "Durable user preferences and workflow notes.",
            "Grounded repository facts and recurring tooling notes.",
        ][self as usize]
    }

    fn slug(self) -> &'static str {
        ["preferences", "repository_facts"][self as usize]
    }

    fn from_slug(value: &str) -> Option<Self> {
        match value {
            "preferences" => Some(Self::Preferences),
            "repository_facts" => Some(Self::RepositoryFacts),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct PersistentMemoryFiles {
    directory: PathBuf,
    private_global: bool,
    summary_file: PathBuf,
    memory_file: PathBuf,
    preferences_file: PathBuf,
    repository_facts_file: PathBuf,
    notes_dir: PathBuf,
    rollout_summaries_dir: PathBuf,
    lock_file: PathBuf,
}

impl PersistentMemoryFiles {
    fn new(directory: PathBuf, private_global: bool) -> Self {
        Self {
            summary_file: directory.join(MEMORY_SUMMARY_FILENAME),
            memory_file: directory.join(MEMORY_FILENAME),
            preferences_file: directory.join(PREFERENCES_FILENAME),
            repository_facts_file: directory.join(REPOSITORY_FACTS_FILENAME),
            notes_dir: directory.join(NOTES_DIRNAME),
            rollout_summaries_dir: directory.join(ROLLOUT_SUMMARIES_DIRNAME),
            lock_file: directory.join(MEMORY_LOCK_FILENAME),
            directory,
            private_global,
        }
    }
}

#[derive(Debug, Clone)]
struct ClassifiedFacts {
    preferences: Vec<GroundedFactRecord>,
    repository_facts: Vec<GroundedFactRecord>,
}

impl ClassifiedFacts {
    fn total(&self) -> usize {
        self.preferences.len() + self.repository_facts.len()
    }
}

/// Resolves the persistent memory directory for a project.
///
/// **Blocking**: This function may resolve filesystem metadata.
/// Callers in async contexts must wrap this in `tokio::task::spawn_blocking`.
pub fn resolve_persistent_memory_dir(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
) -> Result<Option<PathBuf>> {
    let project_name = persistent_memory_project_name(workspace_root);
    let directory = persistent_memory_base_dir(config)?
        .join("projects")
        .join(sanitize_project_name(&project_name))
        .join("memory");
    Ok(Some(directory))
}

/// Returns the current persistent memory status.
///
/// **Blocking**: This function performs filesystem I/O (directory migration,
/// file existence checks, reading topic files). Callers in async contexts
/// must wrap this in `tokio::task::spawn_blocking`.
pub fn persistent_memory_status(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
) -> Result<PersistentMemoryStatus> {
    let directory = resolve_persistent_memory_dir(config, workspace_root)?
        .ok_or_else(|| anyhow!("persistent memory path policy returned no storage directory"))?;
    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    let pending_rollout_summaries = count_pending_rollout_summaries(&files.rollout_summaries_dir)?;
    let cleanup_status = detect_memory_cleanup_status(&files)?;

    Ok(PersistentMemoryStatus {
        enabled: config.enabled,
        auto_write: config.auto_write,
        summary_exists: is_regular_file(&files.summary_file),
        registry_exists: is_regular_file(&files.memory_file),
        pending_rollout_summaries,
        cleanup_status,
        directory: files.directory,
        summary_file: files.summary_file,
        memory_file: files.memory_file,
        preferences_file: files.preferences_file,
        repository_facts_file: files.repository_facts_file,
        notes_dir: files.notes_dir,
        rollout_summaries_dir: files.rollout_summaries_dir,
    })
}

pub async fn read_persistent_memory_excerpt(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
) -> Result<Option<PersistentMemoryExcerpt>> {
    if !config.enabled {
        return Ok(None);
    }

    let config_clone = config.clone();
    let workspace_root = workspace_root.to_path_buf();
    let status = tokio::task::spawn_blocking(move || persistent_memory_status(&config_clone, &workspace_root))
        .await
        .context("Persistent memory status task panicked")??;
    if !tokio::fs::try_exists(&status.summary_file).await.unwrap_or(false) {
        return Ok(None);
    }

    let raw = String::from_utf8(diapason_commons::fs::read_private_file_no_follow(&status.summary_file).await?)
        .with_context(|| format!("Failed to decode persistent memory summary {}", status.summary_file.display()))?;

    let (contents, truncated, bytes_read, lines_read) =
        truncate_memory_excerpt(&raw, config.startup_line_limit, config.startup_byte_limit);

    Ok(Some(PersistentMemoryExcerpt {
        status,
        contents,
        truncated,
        bytes_read,
        lines_read,
    }))
}

pub async fn read_persistent_memory_excerpt_for_config(
    vt_cfg: Option<&DiapasonConfig>,
    workspace_root: &Path,
) -> Result<Option<PersistentMemoryExcerpt>> {
    let config = effective_persistent_memory_config(vt_cfg);
    read_persistent_memory_excerpt(&config, workspace_root).await
}

pub async fn finalize_persistent_memory(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    history: &[Message],
) -> Result<Option<PersistentMemoryWriteReport>> {
    let config = effective_generated_memory_config(vt_cfg);
    if !config.enabled || !config.auto_write {
        return Ok(None);
    }
    let cfg_status = config.clone();
    let ws_status = runtime_config.workspace.clone();
    if tokio::task::spawn_blocking(move || persistent_memory_status(&cfg_status, ws_status.as_path()))
        .await
        .context("Persistent memory status task panicked")??
        .cleanup_status
        .needed
    {
        return Ok(None);
    }

    let facts = dedup_latest_facts(history, DEFAULT_FACT_LIMIT);
    persist_memory_internal(
        &config,
        runtime_config.workspace.as_path(),
        Some(runtime_config),
        vt_cfg,
        FactsInput::Candidates(&facts),
        true,
        false,
    )
    .await
}

pub async fn rebuild_persistent_memory_summary(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
) -> Result<Option<PersistentMemoryWriteReport>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled {
        return Ok(None);
    }
    let cfg_rb = config.clone();
    let ws_rb = runtime_config.workspace.clone();
    if tokio::task::spawn_blocking(move || persistent_memory_status(&cfg_rb, ws_rb.as_path()))
        .await
        .context("Persistent memory status task panicked")??
        .cleanup_status
        .needed
    {
        bail!("persistent memory cleanup is required before rebuilding the summary");
    }

    persist_memory_internal(
        &config,
        runtime_config.workspace.as_path(),
        Some(runtime_config),
        vt_cfg,
        FactsInput::Candidates(&[]),
        false,
        true,
    )
    .await
}

pub async fn rebuild_generated_memory_files(config: &PersistentMemoryConfig, workspace_root: &Path) -> Result<()> {
    let cfg = config.clone();
    let ws = workspace_root.to_path_buf();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg, &ws))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve")?;
    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    let mut created_files = Vec::new();
    ensure_memory_layout(&files, &mut created_files).await?;
    let _lock = MemoryLock::acquire(&files.lock_file).await?;
    // Consolidation is best-effort; log errors but don't fail initialization
    if let Err(e) = consolidate_memory_files(None, None, workspace_root, &files).await {
        tracing::warn!("Failed to consolidate memory files during init: {}", e);
    }
    Ok(())
}

pub async fn scaffold_persistent_memory(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
) -> Result<Option<PersistentMemoryStatus>> {
    let cfg = config.clone();
    let ws = workspace_root.to_path_buf();
    let status = tokio::task::spawn_blocking(move || persistent_memory_status(&cfg, &ws))
        .await
        .context("Persistent memory status task panicked")??;
    let files = PersistentMemoryFiles::new(status.directory.clone(), config.directory_override.is_none());
    let mut created_files = Vec::new();
    ensure_memory_layout(&files, &mut created_files).await?;
    let cfg2 = config.clone();
    let ws2 = workspace_root.to_path_buf();
    let final_status = tokio::task::spawn_blocking(move || persistent_memory_status(&cfg2, &ws2))
        .await
        .context("Persistent memory status task panicked")??;
    Ok(Some(final_status))
}

/// Write classified facts to all memory files (topic files, index, summary).
async fn write_classified_memory(
    files: &PersistentMemoryFiles,
    classified: &ClassifiedFacts,
    notes: &[MemoryNoteSummary],
    runtime_config: Option<&RuntimeAgentConfig>,
    vt_cfg: Option<&DiapasonConfig>,
    workspace_root: &Path,
) -> Result<Vec<PathBuf>> {
    let mut created_files = Vec::new();
    async fn write_memory_file(
        path: &Path,
        contents: String,
        created_files: &mut Vec<PathBuf>,
        private_global: bool,
    ) -> Result<()> {
        if !tokio::fs::try_exists(path).await.unwrap_or(false) {
            created_files.push(path.to_path_buf());
        }
        if private_global {
            diapason_commons::fs::write_private_file_atomic(path, contents.as_bytes())
                .await
                .with_context(|| format!("Failed to write private memory file {}", path.display()))
        } else {
            tokio::fs::write(path, contents)
                .await
                .with_context(|| format!("Failed to write {}", path.display()))
        }
    }
    write_memory_file(
        &files.preferences_file,
        render_topic_file(MemoryTopic::Preferences, &classified.preferences),
        &mut created_files,
        files.private_global,
    )
    .await?;
    write_memory_file(
        &files.repository_facts_file,
        render_topic_file(MemoryTopic::RepositoryFacts, &classified.repository_facts),
        &mut created_files,
        files.private_global,
    )
    .await?;
    write_memory_file(
        &files.memory_file,
        render_memory_index(&classified.preferences, &classified.repository_facts, notes, 0),
        &mut created_files,
        files.private_global,
    )
    .await?;
    let summary = summarize_memory(
        runtime_config,
        vt_cfg,
        workspace_root,
        &classified.preferences,
        &classified.repository_facts,
        notes,
    )
    .await
    .unwrap_or_else(|| render_memory_summary(&classified.preferences, &classified.repository_facts, notes));
    write_memory_file(&files.summary_file, summary, &mut created_files, files.private_global).await?;
    Ok(created_files)
}

pub async fn cleanup_persistent_memory(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    include_summary_only_signals: bool,
) -> Result<Option<PersistentMemoryCleanupReport>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled {
        return Ok(None);
    }

    let cfg_dir = config.clone();
    let ws_dir = runtime_config.workspace.clone();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg_dir, ws_dir.as_path()))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve when enabled")?;
    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    let mut created_files = Vec::new();
    ensure_memory_layout(&files, &mut created_files).await?;

    let status = detect_memory_cleanup_status_async(&files).await?;
    if !status.needed && !include_summary_only_signals {
        return Ok(Some(PersistentMemoryCleanupReport {
            directory: files.directory,
            summary_file: files.summary_file,
            memory_file: files.memory_file,
            rewritten_facts: 0,
            removed_rollout_files: 0,
        }));
    }

    let _lock = MemoryLock::acquire(&files.lock_file).await?;
    let (candidates, notes) =
        tokio::try_join!(collect_cleanup_candidates(&files), read_note_summaries(&files.notes_dir))?;
    let classified = if candidates.is_empty() {
        ClassifiedFacts {
            preferences: Vec::new(),
            repository_facts: Vec::new(),
        }
    } else {
        classify_facts_strict(Some(runtime_config), vt_cfg, runtime_config.workspace.as_path(), &candidates).await?
    };

    let removed_rollout_files = remove_rollout_markdown_files(&files.rollout_summaries_dir).await?;
    // Write is critical; propagate errors
    write_classified_memory(
        &files,
        &classified,
        &notes,
        Some(runtime_config),
        vt_cfg,
        runtime_config.workspace.as_path(),
    )
    .await?;

    Ok(Some(PersistentMemoryCleanupReport {
        directory: files.directory,
        summary_file: files.summary_file,
        memory_file: files.memory_file,
        rewritten_facts: classified.total(),
        removed_rollout_files,
    }))
}

pub async fn list_persistent_memory_candidates(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
) -> Result<Option<Vec<PersistentMemoryMatch>>> {
    if !config.enabled {
        return Ok(None);
    }

    let cfg = config.clone();
    let ws = workspace_root.to_path_buf();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg, &ws))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve when enabled")?;
    if !tokio::fs::try_exists(&directory).await.unwrap_or(false) {
        return Ok(Some(Vec::new()));
    }

    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    collect_all_memory_matches(&files).await.map(Some)
}

pub async fn find_persistent_memory_matches(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
    query: &str,
) -> Result<Option<Vec<PersistentMemoryMatch>>> {
    if !config.enabled {
        return Ok(None);
    }
    let Some(normalized_query) = normalize_memory_query(query) else {
        return Ok(Some(Vec::new()));
    };
    let cfg = config.clone();
    let ws = workspace_root.to_path_buf();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg, &ws))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve when enabled")?;
    if !tokio::fs::try_exists(&directory).await.unwrap_or(false) {
        return Ok(Some(Vec::new()));
    }

    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    collect_memory_matches(&files, &normalized_query).await.map(Some)
}

/// Plan a user-approved persistent-memory save.
///
/// `prior_assistant_reply` is reference material for deictic requests such as
/// "remember it" and must contain only the immediately preceding assistant
/// reply. The caller remains responsible for limiting it to those requests and
/// for obtaining confirmation before calling [`persist_remembered_memory_plan`].
pub async fn plan_remember_persistent_memory(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    request: &str,
    supplemental_answer: Option<&str>,
    prior_assistant_reply: Option<&str>,
) -> Result<Option<MemoryOpPlan>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled {
        return Ok(None);
    }

    let plan = plan_memory_operation(
        runtime_config,
        vt_cfg,
        runtime_config.workspace.as_path(),
        MemoryOpKind::Remember,
        request,
        supplemental_answer,
        prior_assistant_reply,
        &[],
    )
    .await?;
    Ok(Some(plan))
}

pub async fn persist_remembered_memory_plan(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    plan: &MemoryOpPlan,
) -> Result<Option<PersistentMemoryWriteReport>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled || plan.kind != MemoryOpKind::Remember {
        return Ok(None);
    }

    let facts = memory_plan_facts(plan)?;
    persist_memory_internal(
        &config,
        runtime_config.workspace.as_path(),
        Some(runtime_config),
        vt_cfg,
        FactsInput::Preclassified(&facts),
        true,
        false,
    )
    .await
}

pub async fn plan_forget_persistent_memory(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    request: &str,
    candidates: &[MemoryOpCandidate],
) -> Result<Option<MemoryOpPlan>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled {
        return Ok(None);
    }

    let plan = plan_memory_operation(
        runtime_config,
        vt_cfg,
        runtime_config.workspace.as_path(),
        MemoryOpKind::Forget,
        request,
        None,
        None,
        candidates,
    )
    .await?;
    Ok(Some(plan))
}

pub async fn forget_planned_persistent_memory_matches(
    runtime_config: &RuntimeAgentConfig,
    vt_cfg: Option<&DiapasonConfig>,
    candidates: &[MemoryOpCandidate],
    plan: &MemoryOpPlan,
) -> Result<Option<PersistentMemoryForgetReport>> {
    let config = effective_persistent_memory_config(vt_cfg);
    if !config.enabled || plan.kind != MemoryOpKind::Forget {
        return Ok(None);
    }

    let selected = selected_memory_candidates(candidates, &plan.selected_ids)?;
    let cfg_dir = config.clone();
    let ws_dir = runtime_config.workspace.clone();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg_dir, ws_dir.as_path()))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve when enabled")?;
    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    if !tokio::fs::try_exists(&files.directory).await.unwrap_or(false) {
        return Ok(Some(PersistentMemoryForgetReport {
            directory: files.directory,
            summary_file: files.summary_file,
            memory_file: files.memory_file,
            removed_facts: 0,
            pending_rollout_summaries: 0,
        }));
    }

    let _lock = MemoryLock::acquire(&files.lock_file).await?;
    let mut removed_facts = 0usize;
    removed_facts += rewrite_topic_without_selected(
        &files.preferences_file,
        MemoryTopic::Preferences,
        &selected,
        files.private_global,
    )
    .await?;
    removed_facts += rewrite_topic_without_selected(
        &files.repository_facts_file,
        MemoryTopic::RepositoryFacts,
        &selected,
        files.private_global,
    )
    .await?;

    let rollout_files = list_rollout_markdown_files_async(&files.rollout_summaries_dir).await?;
    for path in rollout_files {
        removed_facts += scrub_rollout_file_by_selection(&path, &selected, files.private_global).await?;
    }

    if removed_facts > 0 {
        // Consolidation is best-effort; log errors but don't fail the forget operation
        if let Err(e) =
            consolidate_memory_files(Some(runtime_config), vt_cfg, runtime_config.workspace.as_path(), &files).await
        {
            tracing::warn!("Failed to consolidate memory files after forget: {}", e);
        }
    }

    Ok(Some(PersistentMemoryForgetReport {
        directory: files.directory,
        summary_file: files.summary_file,
        memory_file: files.memory_file,
        removed_facts,
        pending_rollout_summaries: count_pending_rollout_summaries_async(&files.rollout_summaries_dir).await?,
    }))
}

/// Distinguishes how facts are provided to the persistence layer.
enum FactsInput<'a> {
    /// Facts already classified into topics (no LLM call needed).
    Preclassified(&'a [GroundedFactRecord]),
    /// Raw candidate facts that must be classified via LLM.
    Candidates(&'a [GroundedFactRecord]),
}

impl FactsInput<'_> {
    fn as_slice(&self) -> &[GroundedFactRecord] {
        match self {
            FactsInput::Preclassified(facts) => facts,
            FactsInput::Candidates(facts) => facts,
        }
    }
}

async fn persist_memory_internal(
    config: &PersistentMemoryConfig,
    workspace_root: &Path,
    runtime_config: Option<&RuntimeAgentConfig>,
    vt_cfg: Option<&DiapasonConfig>,
    facts_input: FactsInput<'_>,
    write_rollout: bool,
    force_rebuild: bool,
) -> Result<Option<PersistentMemoryWriteReport>> {
    let cfg = config.clone();
    let ws = workspace_root.to_path_buf();
    let directory = tokio::task::spawn_blocking(move || resolve_persistent_memory_dir(&cfg, &ws))
        .await
        .context("Persistent memory directory resolution task panicked")??
        .context("persistent memory directory should resolve when enabled")?;
    let files = PersistentMemoryFiles::new(directory, config.directory_override.is_none());
    let mut created_files = Vec::new();
    ensure_memory_layout(&files, &mut created_files).await?;

    let facts_slice = facts_input.as_slice();
    if detect_memory_cleanup_status_async(&files).await?.needed && (write_rollout || !facts_slice.is_empty()) {
        bail!("persistent memory cleanup is required before mutating memory");
    }

    let _lock = MemoryLock::acquire(&files.lock_file).await?;
    let existing_lines = read_existing_memory_lines(&files.directory).await?;
    let deduped_records: Vec<GroundedFactRecord> = facts_slice
        .iter()
        .filter(|f| !existing_lines.contains(&normalize_whitespace(&f.fact).to_ascii_lowercase()))
        .cloned()
        .collect();

    let classified = match facts_input {
        FactsInput::Preclassified(_) => classified_facts_from_records(&deduped_records),
        FactsInput::Candidates(_) if deduped_records.is_empty() => ClassifiedFacts {
            preferences: Vec::new(),
            repository_facts: Vec::new(),
        },
        FactsInput::Candidates(_) => {
            classify_facts_strict(runtime_config, vt_cfg, workspace_root, &deduped_records).await?
        }
    };

    let staged_rollout = if write_rollout && classified.total() > 0 {
        Some(
            write_rollout_summary_pending(&files.rollout_summaries_dir, &classified, files.private_global)
                .await
                .with_context(|| {
                    format!("Failed to write rollout summary under {}", files.rollout_summaries_dir.display())
                })?,
        )
    } else {
        None
    };

    let pending_before = list_pending_rollout_files_async(&files.rollout_summaries_dir).await?;
    let (summary_exists, memory_exists) =
        tokio::try_join!(tokio::fs::try_exists(&files.summary_file), tokio::fs::try_exists(&files.memory_file),)?;
    let should_consolidate =
        force_rebuild || staged_rollout.is_some() || !pending_before.is_empty() || !summary_exists || !memory_exists;
    if !should_consolidate {
        return Ok(None);
    }

    let consolidated = consolidate_memory_files(runtime_config, vt_cfg, workspace_root, &files).await?;
    created_files.extend(consolidated.created_files);
    created_files.sort();
    created_files.dedup();

    Ok(Some(PersistentMemoryWriteReport {
        directory: files.directory,
        summary_file: files.summary_file,
        memory_file: files.memory_file,
        rollout_summary_file: staged_rollout.map(finalize_rollout_summary_path),
        created_files,
        added_facts: consolidated.added_facts,
        pending_rollout_summaries: count_pending_rollout_summaries_async(&files.rollout_summaries_dir).await?,
    }))
}

fn classified_facts_from_records(records: &[GroundedFactRecord]) -> ClassifiedFacts {
    let mut preferences = Vec::new();
    let mut repository_facts = Vec::new();
    for fact in records {
        let topic = decode_topic_source(&fact.source).0.unwrap_or_else(|| classify_fact(fact));
        match topic {
            MemoryTopic::Preferences => preferences.push(fact.clone()),
            MemoryTopic::RepositoryFacts => repository_facts.push(fact.clone()),
        }
    }
    ClassifiedFacts {
        preferences: merge_topic_facts(preferences),
        repository_facts: merge_topic_facts(repository_facts),
    }
}

async fn ensure_memory_layout(files: &PersistentMemoryFiles, created_files: &mut Vec<PathBuf>) -> Result<()> {
    async fn ensure_file(
        path: &Path,
        contents: String,
        created_files: &mut Vec<PathBuf>,
        private_global: bool,
    ) -> Result<()> {
        if private_global {
            match tokio::fs::symlink_metadata(path).await {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    bail!("Refusing symlinked persistent memory file {}", path.display());
                }
                Ok(metadata) if !metadata.is_file() => {
                    bail!("Persistent memory path is not a regular file: {}", path.display());
                }
                Ok(_) => return Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| format!("Failed to inspect {}", path.display()));
                }
            }
        } else if tokio::fs::try_exists(path).await.unwrap_or(false) {
            return Ok(());
        }
        if private_global {
            diapason_commons::fs::write_private_file_atomic(path, contents.as_bytes())
                .await
                .with_context(|| format!("Failed to write private memory file {}", path.display()))?;
        } else {
            tokio::fs::write(path, contents)
                .await
                .with_context(|| format!("Failed to write {}", path.display()))?;
        }
        created_files.push(path.to_path_buf());
        Ok(())
    }
    for (dir, desc) in [
        (&files.directory, "persistent memory"),
        (&files.rollout_summaries_dir, "rollout summaries"),
        (&files.notes_dir, "notes"),
    ] {
        if files.private_global {
            DiapasonPaths::ensure_user_dir(dir)
                .with_context(|| format!("Failed to create {desc} {}", dir.display()))?;
        } else {
            tokio::fs::create_dir_all(dir)
                .await
                .with_context(|| format!("Failed to create {desc} {}", dir.display()))?;
        }
    }
    ensure_file(
        &files.preferences_file,
        render_topic_file(MemoryTopic::Preferences, &[]),
        created_files,
        files.private_global,
    )
    .await?;
    ensure_file(
        &files.repository_facts_file,
        render_topic_file(MemoryTopic::RepositoryFacts, &[]),
        created_files,
        files.private_global,
    )
    .await?;
    ensure_file(&files.memory_file, render_memory_index(&[], &[], &[], 0), created_files, files.private_global).await?;
    ensure_file(&files.summary_file, render_memory_summary(&[], &[], &[]), created_files, files.private_global).await?;
    Ok(())
}

fn is_regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
}

fn truncate_memory_excerpt(contents: &str, line_limit: usize, byte_limit: usize) -> (String, bool, usize, usize) {
    let all_lines = contents.lines().collect::<Vec<_>>();
    let mut selected = String::new();
    let mut bytes_read = 0usize;
    let mut lines_read = 0usize;
    let mut truncated = false;
    for (index, line) in all_lines.iter().enumerate() {
        if lines_read >= line_limit {
            truncated = true;
            break;
        }
        let line_bytes = line.len();
        let trailing_newline = usize::from(index + 1 < all_lines.len());
        if bytes_read + line_bytes + trailing_newline > byte_limit {
            truncated = true;
            break;
        }
        selected.push_str(line);
        selected.push('\n');
        bytes_read += line_bytes + trailing_newline;
        lines_read += 1;
    }
    if !truncated && contents.len() > bytes_read {
        truncated = true;
    }
    (selected.trim_end().to_string(), truncated, bytes_read, lines_read)
}

async fn read_existing_memory_lines(directory: &Path) -> Result<BTreeSet<String>> {
    let mut lines = BTreeSet::new();
    if !tokio::fs::try_exists(directory).await.unwrap_or(false) {
        return Ok(lines);
    }
    let mut stack = vec![directory.to_path_buf()];
    while let Some(next_dir) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&next_dir)
            .await
            .with_context(|| format!("Failed to list {}", next_dir.display()))?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if entry.metadata().await?.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|v| v.to_str()) != Some("md") {
                continue;
            }
            let content = String::from_utf8(diapason_commons::fs::read_private_file_no_follow(&path).await?)
                .with_context(|| format!("failed to decode note file at {}", path.display()))?;
            for line in content.lines() {
                if let Some((_, fact)) = parse_fact_line(line) {
                    lines.insert(normalize_whitespace(&fact).to_ascii_lowercase());
                }
            }
        }
    }
    Ok(lines)
}

const CLEANUP_REMEMBER_MARKERS: &[&str] = &[
    "save to memory",
    "remember that",
    "remember my",
    "remember ",
    "add to memory",
    "store in memory",
];
const CLEANUP_FORGET_MARKERS: &[&str] = &["forget ", "remove from memory", "delete from memory"];
const STRIP_PREFIXES: &[&str] = &[
    "please ",
    "please, ",
    "can you ",
    "could you ",
    "would you ",
    "diapason, ",
    "diapason ",
];
const CLEANUP_NOTE_PREFIXES: &[&str] = &["note that ", "important:"];
const SELF_FACT_PREFIXES: &[&str] = &[
    "my name is ",
    "i prefer ",
    "my preferred ",
    "my pronouns are ",
    "my timezone is ",
];

fn detect_memory_cleanup_status(files: &PersistentMemoryFiles) -> Result<MemoryCleanupStatus> {
    if !files.directory.exists() {
        return Ok(MemoryCleanupStatus {
            needed: false,
            suspicious_facts: 0,
            suspicious_summary_lines: 0,
        });
    }
    let mut suspicious_facts = 0usize;
    for path in [
        &files.preferences_file,
        &files.repository_facts_file,
        &files.memory_file,
    ] {
        suspicious_facts += count_suspicious_facts_in_file(path)?;
    }
    suspicious_facts += count_suspicious_rollout_facts(&files.rollout_summaries_dir)?;
    let suspicious_summary_lines = count_suspicious_summary_lines(&files.summary_file)?;
    Ok(MemoryCleanupStatus {
        needed: suspicious_facts > 0 || suspicious_summary_lines > 0,
        suspicious_facts,
        suspicious_summary_lines,
    })
}

async fn detect_memory_cleanup_status_async(files: &PersistentMemoryFiles) -> Result<MemoryCleanupStatus> {
    let files = files.clone();
    tokio::task::spawn_blocking(move || detect_memory_cleanup_status(&files))
        .await
        .context("Persistent memory cleanup status task panicked")?
}

fn count_suspicious_facts_in_file(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let content = String::from_utf8(DiapasonPaths::read_file_no_follow(path)?)
        .with_context(|| format!("Failed to decode {} as UTF-8", path.display()))?;
    Ok(parse_topic_file(&content).into_iter().filter(is_legacy_polluted_fact).count())
}

fn count_suspicious_rollout_facts(rollout_dir: &Path) -> Result<usize> {
    if !rollout_dir.exists() {
        return Ok(0);
    }
    let mut count = 0usize;
    for entry in std::fs::read_dir(rollout_dir).with_context(|| format!("Failed to list {}", rollout_dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|v| v.to_str()) == Some("md") {
            count += count_suspicious_facts_in_file(&path)?;
        }
    }
    Ok(count)
}

fn count_suspicious_summary_lines(path: &Path) -> Result<usize> {
    if !path.exists() {
        return Ok(0);
    }
    let content = String::from_utf8(DiapasonPaths::read_file_no_follow(path)?)
        .with_context(|| format!("Failed to decode {} as UTF-8", path.display()))?;
    Ok(content
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("- "))
        .map(|l| l.trim_start_matches("- ").trim())
        .filter(|l| looks_like_legacy_prompt(l) || looks_like_serialized_payload(l))
        .count())
}

#[cold]
fn is_legacy_polluted_fact(fact: &GroundedFactRecord) -> bool {
    looks_like_legacy_prompt(&fact.fact) || looks_like_serialized_payload(&fact.fact)
}

#[cold]
fn looks_like_legacy_prompt(text: &str) -> bool {
    let mut lowered = normalize_whitespace(text).to_ascii_lowercase();
    while let Some(stripped) = STRIP_PREFIXES.iter().find_map(|p| lowered.strip_prefix(p)) {
        lowered = stripped.trim_start().to_string();
    }
    CLEANUP_REMEMBER_MARKERS
        .iter()
        .chain(CLEANUP_FORGET_MARKERS.iter())
        .any(|m| lowered.starts_with(m))
}

#[cold]
fn looks_like_serialized_payload(text: &str) -> bool {
    let t = text.trim();
    t.starts_with('{')
        || t.starts_with('[')
        || t.contains("\"query\":")
        || t.contains("\"matches\":")
        || t.contains("\"path\":")
        || t.contains("</parameter>")
        || t.contains("</invoke>")
        || t.contains("<</invoke>")
}

async fn write_rollout_summary_pending(
    rollout_dir: &Path,
    classified: &ClassifiedFacts,
    private_global: bool,
) -> Result<PathBuf> {
    if private_global {
        DiapasonPaths::ensure_user_dir(rollout_dir)
            .with_context(|| format!("Failed to create {}", rollout_dir.display()))?;
    } else {
        tokio::fs::create_dir_all(rollout_dir)
            .await
            .with_context(|| format!("Failed to create {}", rollout_dir.display()))?;
    }
    let path = rollout_dir.join(format!("{}.pending.md", unique_rollout_id()));
    let contents = render_rollout_summary(classified);
    if private_global {
        diapason_commons::fs::write_private_file_atomic(&path, contents.as_bytes())
            .await
            .with_context(|| format!("Failed to write private memory rollout summary {}", path.display()))?;
    } else {
        tokio::fs::write(&path, contents)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(path)
}

fn finalize_rollout_summary_path(path: PathBuf) -> PathBuf {
    match path.file_name().and_then(|v| v.to_str()) {
        Some(name) => path.with_file_name(name.trim_end_matches(".pending.md").to_string() + ".md"),
        None => path,
    }
}

struct ConsolidationResult {
    created_files: Vec<PathBuf>,
    added_facts: usize,
}

async fn consolidate_memory_files(
    runtime_config: Option<&RuntimeAgentConfig>,
    vt_cfg: Option<&DiapasonConfig>,
    workspace_root: &Path,
    files: &PersistentMemoryFiles,
) -> Result<ConsolidationResult> {
    let (pending_files, prefs_existing, repo_existing, rollout, notes) = tokio::try_join!(
        list_pending_rollout_files_async(&files.rollout_summaries_dir),
        read_topic_records(&files.preferences_file, MemoryTopic::Preferences),
        read_topic_records(&files.repository_facts_file, MemoryTopic::RepositoryFacts),
        read_rollout_records(&files.rollout_summaries_dir),
        read_note_summaries(&files.notes_dir),
    )?;
    let classified = ClassifiedFacts {
        preferences: merge_topic_facts(prefs_existing.into_iter().chain(rollout.0).collect()),
        repository_facts: merge_topic_facts(repo_existing.into_iter().chain(rollout.1).collect()),
    };
    let created_files =
        write_classified_memory(files, &classified, &notes, runtime_config, vt_cfg, workspace_root).await?;
    let mut added_facts = 0usize;
    for p in &pending_files {
        if let Ok(c) = String::from_utf8(diapason_commons::fs::read_private_file_no_follow(p).await?) {
            added_facts += c.lines().filter_map(parse_fact_line).count();
        }
    }
    for pending in &pending_files {
        let finalized = finalize_rollout_summary_path(pending.clone());
        if !tokio::fs::try_exists(&finalized).await.unwrap_or(false) {
            tokio::fs::rename(pending, &finalized)
                .await
                .with_context(|| format!("Failed to finalize rollout summary {}", pending.display()))?;
        } else {
            tokio::fs::remove_file(pending)
                .await
                .with_context(|| format!("Failed to remove {}", pending.display()))?;
        }
    }
    Ok(ConsolidationResult { created_files, added_facts })
}

fn merge_topic_facts(records: Vec<GroundedFactRecord>) -> Vec<GroundedFactRecord> {
    let mut facts = Vec::new();
    for fact in records {
        let normalized = normalize_whitespace(&fact.fact).to_ascii_lowercase();
        if let Some(i) = facts
            .iter()
            .position(|e: &GroundedFactRecord| normalize_whitespace(&e.fact).to_ascii_lowercase() == normalized)
        {
            facts.remove(i);
        }
        facts.push(fact);
    }
    let skip = facts.len().saturating_sub(TOPIC_FACT_LIMIT);
    facts.into_iter().skip(skip).collect()
}

fn normalized_selection_key(source: &str, fact: &str) -> String {
    format!(
        "{}::{}",
        normalize_whitespace(source).to_ascii_lowercase(),
        normalize_whitespace(fact).to_ascii_lowercase()
    )
}

fn selection_key_for_record(record: &GroundedFactRecord) -> String {
    let (_topic, source) = decode_topic_source(&record.source);
    normalized_selection_key(&source, &record.fact)
}

fn selection_keys(selected: &[MemoryOpCandidate]) -> BTreeSet<String> {
    selected.iter().map(|e| normalized_selection_key(&e.source, &e.fact)).collect()
}

async fn rewrite_topic_without_selected(
    path: &Path,
    topic: MemoryTopic,
    selected: &[MemoryOpCandidate],
    private_global: bool,
) -> Result<usize> {
    if !tokio::fs::try_exists(path).await.unwrap_or(false) {
        return Ok(0);
    }
    let keys = selection_keys(selected);
    let facts = read_topic_records(path, topic).await?;
    let removed = facts.iter().filter(|f| keys.contains(&selection_key_for_record(f))).count();
    if removed == 0 {
        return Ok(0);
    }
    let kept: Vec<_> = facts
        .into_iter()
        .filter(|f| !keys.contains(&selection_key_for_record(f)))
        .collect();
    let contents = render_topic_file(topic, &kept);
    if private_global {
        diapason_commons::fs::write_private_file_atomic(path, contents.as_bytes())
            .await
            .with_context(|| format!("Failed to write private memory file {}", path.display()))?;
    } else {
        tokio::fs::write(path, contents)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(removed)
}

async fn scrub_rollout_file_by_selection(
    path: &Path,
    selected: &[MemoryOpCandidate],
    private_global: bool,
) -> Result<usize> {
    let contents = String::from_utf8(diapason_commons::fs::read_private_file_no_follow(path).await?)
        .with_context(|| format!("Failed to decode {} as UTF-8", path.display()))?;
    let keys = selection_keys(selected);
    let mut removed = 0usize;
    let mut filtered = Vec::new();
    for line in contents.lines() {
        let keep = parse_fact_line(line).is_none_or(|(source, fact)| {
            let m = keys.contains(&selection_key_for_record(&GroundedFactRecord { source, fact }));
            if m {
                removed += 1;
            }
            !m
        });
        if keep {
            filtered.push(line);
        }
    }
    if removed == 0 {
        return Ok(0);
    }
    let mut rewritten = filtered.join("\n");
    if contents.ends_with('\n') {
        rewritten.push('\n');
    }
    if private_global {
        diapason_commons::fs::write_private_file_atomic(path, rewritten.as_bytes())
            .await
            .with_context(|| format!("Failed to write private memory rollout summary {}", path.display()))?;
    } else {
        tokio::fs::write(path, rewritten)
            .await
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(removed)
}

async fn remove_rollout_markdown_files(rollout_dir: &Path) -> Result<usize> {
    let files = list_rollout_markdown_files(rollout_dir)?;
    let count = files.len();
    for p in files {
        tokio::fs::remove_file(&p)
            .await
            .with_context(|| format!("Failed to remove {}", p.display()))?;
    }
    Ok(count)
}

fn memory_plan_facts(plan: &MemoryOpPlan) -> Result<Vec<GroundedFactRecord>> {
    if plan.kind != MemoryOpKind::Remember {
        bail!("memory plan is not a remember operation");
    }
    Ok(plan
        .facts
        .iter()
        .map(|f| {
            let topic = match f.topic {
                MemoryPlannedTopic::Preferences => MemoryTopic::Preferences,
                MemoryPlannedTopic::RepositoryFacts => MemoryTopic::RepositoryFacts,
            };
            let source = if f.source.trim().is_empty() {
                "manual_memory".to_string()
            } else {
                normalize_whitespace(&f.source)
            };
            GroundedFactRecord {
                fact: truncate_for_fact(&normalize_whitespace(&f.fact), 180),
                source: encode_topic_source(topic, &source),
            }
        })
        .filter(|f| !f.fact.is_empty())
        .collect())
}

fn selected_memory_candidates(
    candidates: &[MemoryOpCandidate],
    selected_ids: &[usize],
) -> Result<Vec<MemoryOpCandidate>> {
    let selected: Vec<_> = selected_ids
        .iter()
        .filter_map(|id| candidates.iter().find(|c| c.id == *id).cloned())
        .collect();
    if selected_ids.len() != selected.len() {
        bail!("memory plan selected a missing candidate");
    }
    Ok(selected)
}

fn effective_persistent_memory_config(vt_cfg: Option<&DiapasonConfig>) -> PersistentMemoryConfig {
    let mut config = vt_cfg.map(|cfg| cfg.agent.persistent_memory.clone()).unwrap_or_default();
    if let Some(cfg) = vt_cfg {
        config.enabled = cfg.persistent_memory_enabled();
    }
    config
}

fn effective_generated_memory_config(vt_cfg: Option<&DiapasonConfig>) -> PersistentMemoryConfig {
    let mut config = effective_persistent_memory_config(vt_cfg);
    if let Some(cfg) = vt_cfg {
        config.enabled = cfg.should_generate_memories();
    }
    config
}

#[cfg(test)]
mod persistent_memory_tests;
