//! Serializes and restores durable session history, metadata, and transcript summaries.
use crate::config::{HistoryPersistence, VTCodeConfig};
use crate::core::agent::snapshots::SnapshotTurnDiagnostics;

mod archive;
mod listing;
mod retention;
mod snapshot_compaction;
mod transcript;

use crate::llm::provider::{AssistantPhase, Message, MessageClearAt, MessageContent, MessageRole, ToolCall};
use crate::utils::error_log_collector::ErrorLogEntry;
use crate::utils::session_transcript_norm::normalize_distinct_tools_for_summary;
use anyhow::{Context, Result};
#[cfg(test)]
use archive::generate_unique_archive_path;
pub use archive::{SessionArchive, generate_session_archive_identifier, reserve_session_archive_identifier};
pub(super) use archive::{is_session_file, session_archive_path_for_identifier, validate_session_identifier};
use chrono::{DateTime, Utc};
#[cfg(test)]
use listing::truncate_preview;
pub use listing::{
    SearchResult, find_session_by_id_or_name, find_session_by_identifier, list_recent_sessions, search_sessions,
    session_listing_matches_workspace, session_workspace_path,
};
pub(super) use retention::apply_session_retention_best_effort;
#[cfg(test)]
use retention::{BYTES_PER_MB, SessionRetentionLimits, apply_session_retention_with_limits};
use serde::{Deserialize, Deserializer, Serialize};
pub(super) use snapshot_compaction::prepare_snapshot_for_write;
#[cfg(test)]
use snapshot_compaction::{
    bound_session_archive_extensions, compact_snapshot_to_max_bytes, serialized_json_size_at_most,
};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
pub(super) use transcript::{clean_transcript_lines, progress_transcript_from_recent_messages};
use uuid::Uuid;
use vtcode_commons::VtCodePaths;

pub const SESSION_DIR_ENV: &str = "VT_SESSION_DIR";
pub const SESSION_MAX_FILES_ENV: &str = "VT_SESSION_MAX_FILES";
pub const SESSION_MAX_AGE_DAYS_ENV: &str = "VT_SESSION_MAX_AGE_DAYS";
pub const SESSION_MAX_SIZE_MB_ENV: &str = "VT_SESSION_MAX_SIZE_MB";
/// Maximum number of namespaced extension values retained in a session archive.
pub const MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES: usize = 64;
/// Maximum serialized JSON size of extension values retained in a session archive.
pub const MAX_SESSION_ARCHIVE_EXTENSION_BYTES: usize = 16 * 1024;
/// Namespace used to preserve unrecognized fields from older archive formats.
pub const SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE: &str = "dev.vtcode.archive.legacy_fields";

#[derive(Debug, Clone, Copy)]
struct SessionHistorySettings {
    persistence: HistoryPersistence,
    max_bytes: Option<usize>,
}

impl Default for SessionHistorySettings {
    fn default() -> Self {
        Self {
            persistence: HistoryPersistence::File,
            max_bytes: None,
        }
    }
}

static SESSION_HISTORY_SETTINGS: OnceLock<Mutex<SessionHistorySettings>> = OnceLock::new();

fn session_history_settings() -> SessionHistorySettings {
    SESSION_HISTORY_SETTINGS
        .get()
        .map(|settings| *settings.lock().unwrap_or_else(|e| e.into_inner()))
        .unwrap_or_default()
}

pub fn apply_session_history_config_from_vtcode(config: &VTCodeConfig) {
    let settings = SessionHistorySettings {
        persistence: config.history.persistence,
        max_bytes: config.history.max_bytes,
    };
    let cell = SESSION_HISTORY_SETTINGS.get_or_init(|| Mutex::new(SessionHistorySettings::default()));
    if let Ok(mut guard) = cell.lock() {
        *guard = settings;
    }
}

pub fn history_persistence_enabled() -> bool {
    matches!(session_history_settings().persistence, HistoryPersistence::File)
}

#[cfg(test)]
mod test_env_overrides {
    use hashbrown::HashMap;
    use std::ffi::OsString;
    use std::sync::{LazyLock, Mutex};

    static OVERRIDES: LazyLock<Mutex<HashMap<String, Option<OsString>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

    pub(super) fn get(key: &str) -> Option<Option<OsString>> {
        OVERRIDES.lock().ok().and_then(|map| map.get(key).cloned())
    }

    pub(super) fn set(key: &str, value: Option<OsString>) {
        if let Ok(mut map) = OVERRIDES.lock() {
            map.insert(key.to_owned(), value);
        }
    }

    pub(super) fn clear(key: &str) {
        if let Ok(mut map) = OVERRIDES.lock() {
            map.remove(key);
        }
    }
}

fn read_env_var_os(key: &str) -> Option<std::ffi::OsString> {
    #[cfg(test)]
    if let Some(override_value) = test_env_overrides::get(key) {
        return override_value;
    }

    env::var_os(key)
}

fn read_env_var(key: &str) -> Option<String> {
    #[cfg(test)]
    if let Some(override_value) = test_env_overrides::get(key) {
        return override_value.map(|value| value.to_string_lossy().to_string());
    }

    env::var(key).ok()
}

#[cfg(test)]
fn set_test_env_override_path(key: &str, value: &Path) {
    test_env_overrides::set(key, Some(value.as_os_str().to_os_string()));
}

#[cfg(test)]
fn clear_test_env_override(key: &str) {
    test_env_overrides::clear(key);
}

#[cfg(test)]
pub(crate) fn override_sessions_dir_for_tests(path: &Path) {
    set_test_env_override_path(SESSION_DIR_ENV, path);
}

#[cfg(test)]
pub(crate) fn clear_sessions_dir_override_for_tests() {
    clear_test_env_override(SESSION_DIR_ENV);
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionArchiveMetadata {
    pub workspace_label: String,
    pub workspace_path: String,
    pub model: String,
    pub provider: String,
    pub theme: String,
    pub reasoning_effort: String,
    /// Opaque values owned by protocol adapters and keyed by stable namespace.
    ///
    /// Core preserves these values without interpreting their contents. The
    /// number of namespaces and their serialized size are bounded by
    /// [`MAX_SESSION_ARCHIVE_EXTENSION_NAMESPACES`] and
    /// [`MAX_SESSION_ARCHIVE_EXTENSION_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<SessionArchiveExtensions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_log_path: Option<String>,
    /// Names of skills loaded in this session
    #[serde(default)]
    pub loaded_skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_lineage_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_mode: Option<SessionForkMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_metadata: Option<SessionContinuationMetadata>,
}

/// Namespaced, opaque values persisted with a session archive.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SessionArchiveExtensions(serde_json::Map<String, serde_json::Value>);

impl SessionArchiveExtensions {
    /// Create an empty extension set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return an extension value for `namespace`.
    pub fn get(&self, namespace: &str) -> Option<&serde_json::Value> {
        self.0.get(namespace)
    }

    /// Return a mutable extension value for `namespace`.
    pub fn get_mut(&mut self, namespace: &str) -> Option<&mut serde_json::Value> {
        self.0.get_mut(namespace)
    }

    /// Insert an opaque extension value, replacing the value in that namespace.
    pub fn insert(&mut self, namespace: impl Into<String>, value: serde_json::Value) -> Option<serde_json::Value> {
        self.0.insert(namespace.into(), value)
    }

    /// Remove and return the extension value for `namespace`.
    pub fn remove(&mut self, namespace: &str) -> Option<serde_json::Value> {
        self.0.remove(namespace)
    }

    /// Iterate over namespaces and their opaque values.
    pub fn iter(&self) -> impl Iterator<Item = (&String, &serde_json::Value)> {
        self.0.iter()
    }

    /// Return the number of namespaces.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Return whether no namespaces are present.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[derive(Deserialize)]
struct SessionArchiveMetadataWire {
    workspace_label: String,
    workspace_path: String,
    model: String,
    provider: String,
    theme: String,
    reasoning_effort: String,
    #[serde(default)]
    extensions: Option<SessionArchiveExtensions>,
    #[serde(default)]
    debug_log_path: Option<String>,
    #[serde(default)]
    loaded_skills: Vec<String>,
    #[serde(default)]
    prompt_cache_lineage_id: Option<String>,
    #[serde(default)]
    external_thread_id: Option<String>,
    #[serde(default)]
    parent_session_id: Option<String>,
    #[serde(default)]
    primary_agent: Option<String>,
    #[serde(default)]
    fork_mode: Option<SessionForkMode>,
    #[serde(default)]
    continuation_metadata: Option<SessionContinuationMetadata>,
    // Unknown top-level values from older writers stay opaque to the core
    // archive contract and are preserved under the generic legacy namespace.
    #[serde(flatten)]
    legacy_fields: serde_json::Map<String, serde_json::Value>,
}

impl<'de> Deserialize<'de> for SessionArchiveMetadata {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = SessionArchiveMetadataWire::deserialize(deserializer)?;
        let had_extensions = wire.extensions.is_some() || !wire.legacy_fields.is_empty();
        let mut extensions = wire.extensions.unwrap_or_default();
        if !wire.legacy_fields.is_empty() {
            let mut legacy_fields = extensions
                .remove(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE)
                .and_then(|value| value.as_object().cloned())
                .unwrap_or_default();
            legacy_fields.extend(wire.legacy_fields);
            drop(extensions.insert(SESSION_ARCHIVE_LEGACY_FIELDS_NAMESPACE, serde_json::Value::Object(legacy_fields)));
        }

        Ok(Self {
            workspace_label: wire.workspace_label,
            workspace_path: wire.workspace_path,
            model: wire.model,
            provider: wire.provider,
            theme: wire.theme,
            reasoning_effort: wire.reasoning_effort,
            extensions: had_extensions.then_some(extensions),
            debug_log_path: wire.debug_log_path,
            loaded_skills: wire.loaded_skills,
            prompt_cache_lineage_id: wire.prompt_cache_lineage_id,
            external_thread_id: wire.external_thread_id,
            parent_session_id: wire.parent_session_id,
            primary_agent: wire.primary_agent,
            fork_mode: wire.fork_mode,
            continuation_metadata: wire.continuation_metadata,
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionForkMode {
    FullCopy,
    Summarized,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionContinuationExhaustionReason {
    MaxBudgetUsd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SessionContinuationRecommendedAction {
    ContinueFromSummary,
    ContinueFullHistory,
    StartFresh,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionContinuationMetadata {
    pub exhaustion_reason: SessionContinuationExhaustionReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_budget_usd_micros: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual_cost_usd_micros: Option<u64>,
    #[serde(default)]
    pub summary_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_action: Option<SessionContinuationRecommendedAction>,
}

impl SessionContinuationMetadata {
    pub fn budget_limit(max_budget_usd: f64, actual_cost_usd: f64, summary_available: bool) -> Self {
        Self {
            exhaustion_reason: SessionContinuationExhaustionReason::MaxBudgetUsd,
            max_budget_usd_micros: usd_to_micros(max_budget_usd),
            actual_cost_usd_micros: usd_to_micros(actual_cost_usd),
            summary_available,
            recommended_action: Some(SessionContinuationRecommendedAction::ContinueFromSummary),
        }
    }

    pub fn max_budget_usd(&self) -> Option<f64> {
        self.max_budget_usd_micros.map(micros_to_usd)
    }

    pub fn actual_cost_usd(&self) -> Option<f64> {
        self.actual_cost_usd_micros.map(micros_to_usd)
    }

    pub fn is_budget_limit(&self) -> bool {
        matches!(self.exhaustion_reason, SessionContinuationExhaustionReason::MaxBudgetUsd)
    }
}

fn usd_to_micros(value: f64) -> Option<u64> {
    if !value.is_finite() || value.is_sign_negative() {
        return None;
    }

    #[allow(
        clippy::cast_sign_loss,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    Some((value * 1_000_000.0).round() as u64)
}

fn micros_to_usd(value: u64) -> f64 {
    value as f64 / 1_000_000.0
}

impl SessionArchiveMetadata {
    pub fn new(
        workspace_label: impl Into<String>,
        workspace_path: impl Into<String>,
        model: impl Into<String>,
        provider: impl Into<String>,
        theme: impl Into<String>,
        reasoning_effort: impl Into<String>,
    ) -> Self {
        Self {
            workspace_label: workspace_label.into(),
            workspace_path: workspace_path.into(),
            model: model.into(),
            provider: provider.into(),
            theme: theme.into(),
            reasoning_effort: reasoning_effort.into(),
            extensions: None,
            debug_log_path: None,
            loaded_skills: Vec::new(),
            prompt_cache_lineage_id: None,
            external_thread_id: None,
            parent_session_id: None,
            primary_agent: None,
            fork_mode: None,
            continuation_metadata: None,
        }
    }

    /// Set loaded skills for this session
    pub fn with_loaded_skills(mut self, skills: Vec<String>) -> Self {
        self.loaded_skills = skills;
        self
    }

    /// Set debug log path associated with this archive.
    pub fn with_debug_log_path(mut self, path: Option<String>) -> Self {
        self.debug_log_path = path;
        self
    }

    pub fn with_prompt_cache_lineage_id(mut self, lineage_id: impl Into<String>) -> Self {
        self.prompt_cache_lineage_id = Some(lineage_id.into());
        self
    }

    pub fn with_external_thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.external_thread_id = Some(thread_id.into());
        self
    }

    pub fn ensure_prompt_cache_lineage_id(mut self) -> Self {
        if self.prompt_cache_lineage_id.is_none() {
            self.prompt_cache_lineage_id = Some(format!("lineage-{}", Uuid::new_v4()));
        }
        self
    }

    pub fn with_parent_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.parent_session_id = Some(session_id.into());
        self
    }

    pub fn with_primary_agent(mut self, primary_agent: impl Into<String>) -> Self {
        self.primary_agent = Some(primary_agent.into());
        self
    }

    pub fn with_fork_mode(mut self, fork_mode: SessionForkMode) -> Self {
        self.fork_mode = Some(fork_mode);
        self
    }

    pub fn with_continuation_metadata(mut self, continuation_metadata: Option<SessionContinuationMetadata>) -> Self {
        self.continuation_metadata = continuation_metadata;
        self
    }

    pub fn budget_limit_continuation(&self) -> Option<&SessionContinuationMetadata> {
        self.continuation_metadata
            .as_ref()
            .filter(|continuation| continuation.is_budget_limit())
    }

    /// Return an opaque value associated with `namespace`.
    pub fn extension(&self, namespace: &str) -> Option<&serde_json::Value> {
        self.extensions.as_ref()?.get(namespace)
    }

    pub fn set_extension(&mut self, namespace: impl Into<String>, value: serde_json::Value) {
        let extensions = self.extensions.get_or_insert_with(SessionArchiveExtensions::new);
        drop(extensions.insert(namespace, value));
    }

    /// Remove and return an opaque value associated with `namespace`.
    pub fn remove_extension(&mut self, namespace: &str) -> Option<serde_json::Value> {
        let extensions = self.extensions.as_mut()?;
        let removed = extensions.remove(namespace);
        if extensions.is_empty() {
            self.extensions = None;
        }
        removed
    }

    fn fork_seed(&self) -> Self {
        Self {
            workspace_label: self.workspace_label.clone(),
            workspace_path: self.workspace_path.clone(),
            model: self.model.clone(),
            provider: self.provider.clone(),
            theme: self.theme.clone(),
            reasoning_effort: self.reasoning_effort.clone(),
            extensions: self.extensions.clone(),
            debug_log_path: self.debug_log_path.clone(),
            loaded_skills: self.loaded_skills.clone(),
            prompt_cache_lineage_id: self.prompt_cache_lineage_id.clone(),
            external_thread_id: self.external_thread_id.clone(),
            parent_session_id: None,
            primary_agent: self.primary_agent.clone(),
            fork_mode: None,
            continuation_metadata: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    pub role: MessageRole,
    pub content: MessageContent,
    // Keep sparse metadata boxed so long histories don't pay large inline Option<T> costs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_string_opt"
    )]
    pub reasoning: Option<Box<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_vec_opt"
    )]
    pub reasoning_details: Option<Box<Vec<serde_json::Value>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_vec_opt"
    )]
    pub tool_calls: Option<Box<Vec<ToolCall>>>,
    #[serde(default, deserialize_with = "deserialize_boxed_non_empty_string_opt")]
    pub tool_call_id: Option<Box<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AssistantPhase>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_string_opt"
    )]
    pub origin_tool: Option<Box<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_metadata_opt"
    )]
    pub metadata: Option<Box<crate::core::message_metadata::MessageMetadata>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clear_at: Option<MessageClearAt>,
}

impl Eq for SessionMessage {}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
#[inline]
fn boxed_non_empty_string(value: Option<String>) -> Option<Box<String>> {
    value.and_then(|value| (!value.is_empty()).then_some(Box::new(value)))
}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
#[inline]
fn boxed_non_empty_vec<T>(value: Option<Vec<T>>) -> Option<Box<Vec<T>>> {
    value.and_then(|value| (!value.is_empty()).then_some(Box::new(value)))
}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
fn clone_non_empty_boxed_string(value: &Option<Box<String>>) -> Option<String> {
    value
        .as_deref()
        .and_then(|value| (!value.is_empty()).then_some(value.to_owned()))
}

fn deserialize_boxed_metadata_opt<'de, D>(
    deserializer: D,
) -> Result<Option<Box<crate::core::message_metadata::MessageMetadata>>, D::Error>
where
    D: Deserializer<'de>,
{
    crate::core::message_metadata::MessageMetadata::deserialize(deserializer).map(|m| Some(Box::new(m)))
}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
fn clone_non_empty_boxed_vec<T: Clone>(value: &Option<Box<Vec<T>>>) -> Option<Vec<T>> {
    value.as_deref().and_then(|value| (!value.is_empty()).then_some(value.clone()))
}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
fn deserialize_boxed_non_empty_string_opt<'de, D>(deserializer: D) -> Result<Option<Box<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(boxed_non_empty_string)
}

#[expect(
    clippy::box_collection,
    reason = "Intentional compatibility, platform, test, or API-shape suppression."
)]
fn deserialize_boxed_non_empty_vec_opt<'de, D, T>(deserializer: D) -> Result<Option<Box<Vec<T>>>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<Vec<T>>::deserialize(deserializer).map(boxed_non_empty_vec)
}

impl SessionMessage {
    fn base(role: MessageRole, content: MessageContent) -> Self {
        Self {
            role,
            content,
            reasoning: None,
            reasoning_details: None,
            tool_calls: None,
            tool_call_id: None,
            phase: None,
            origin_tool: None,
            metadata: None,
            clear_at: None,
        }
    }

    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self::base(role, MessageContent::Text(content.into()))
    }

    pub fn with_content(role: MessageRole, content: MessageContent) -> Self {
        Self::base(role, content)
    }

    pub fn with_tool_call_id(role: MessageRole, content: impl Into<String>, tool_call_id: Option<String>) -> Self {
        Self::with_tool_call_id_content(role, MessageContent::Text(content.into()), tool_call_id)
    }

    pub fn with_tool_call_id_content(role: MessageRole, content: MessageContent, tool_call_id: Option<String>) -> Self {
        let mut message = Self::base(role, content);
        message.tool_call_id = boxed_non_empty_string(tool_call_id);
        message
    }
}

impl From<&Message> for SessionMessage {
    fn from(message: &Message) -> Self {
        Self {
            role: message.role,
            content: message.content.clone(),
            reasoning: boxed_non_empty_string(message.reasoning.clone()),
            reasoning_details: boxed_non_empty_vec(message.reasoning_details.clone()),
            tool_calls: boxed_non_empty_vec(message.tool_calls.clone()),
            tool_call_id: boxed_non_empty_string(message.tool_call_id.clone()),
            phase: message.phase,
            origin_tool: boxed_non_empty_string(message.origin_tool.clone()),
            metadata: message.metadata.clone().map(Box::new),
            clear_at: message.clear_at,
        }
    }
}

impl From<&SessionMessage> for Message {
    fn from(message: &SessionMessage) -> Self {
        Self {
            role: message.role,
            content: message.content.clone(),
            reasoning: clone_non_empty_boxed_string(&message.reasoning),
            reasoning_details: clone_non_empty_boxed_vec(&message.reasoning_details),
            tool_calls: clone_non_empty_boxed_vec(&message.tool_calls),
            tool_call_id: clone_non_empty_boxed_string(&message.tool_call_id),
            phase: message.phase,
            origin_tool: clone_non_empty_boxed_string(&message.origin_tool),
            metadata: message.metadata.as_deref().cloned(),
            clear_at: message.clear_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSnapshot {
    pub metadata: SessionArchiveMetadata,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub total_messages: usize,
    pub distinct_tools: Vec<String>,
    pub transcript: Vec<String>,
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    // SessionProgress is heavy and frequently absent in final snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<Box<SessionProgress>>,
    /// ERROR-level log entries captured during the session for post-mortem debugging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_logs: Vec<ErrorLogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionProgress {
    pub turn_number: usize,
    #[serde(default)]
    pub recent_messages: Vec<SessionMessage>,
    #[serde(default)]
    pub tool_summaries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    /// Names of skills loaded at checkpoint time
    #[serde(default)]
    pub loaded_skills: Vec<String>,
    /// Diagnostics for the most recently checkpointed turn, including turns
    /// that ended blocked, cancelled, or aborted before a turn snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_diagnostics: Option<SnapshotTurnDiagnostics>,
}

#[derive(Debug, Clone)]
pub struct SessionListing {
    pub path: PathBuf,
    pub snapshot: SessionSnapshot,
}

/// An archive identifier derived from a loaded session listing.
///
/// The inner value is crate-private so callers cannot mark an arbitrary
/// runtime/session ID as resumable. Obtain one from
/// [`SessionArchive::verify_persisted_resume_identifier`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSessionArchiveIdentifier(pub(crate) String);

impl VerifiedSessionArchiveIdentifier {
    /// Borrow the verified archive identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone)]
pub struct SessionProgressArgs {
    pub total_messages: usize,
    pub distinct_tools: Vec<String>,
    pub messages: Vec<SessionMessage>,
    pub recent_messages: Vec<SessionMessage>,
    pub turn_number: usize,
    pub token_usage: Option<String>,
    pub max_context_tokens: Option<usize>,
    pub loaded_skills: Option<Vec<String>>,
    /// Optional diagnostics for the turn represented by this checkpoint.
    pub turn_diagnostics: Option<SnapshotTurnDiagnostics>,
}

/// Outcome of an asynchronous session-progress checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionProgressPersistenceStatus {
    /// The archive file was written successfully.
    Persisted(PathBuf),
    /// The call was intentionally skipped by the progress throttle.
    Throttled(PathBuf),
    /// History persistence is disabled, so no archive file was written.
    Disabled(PathBuf),
}

impl SessionProgressPersistenceStatus {
    /// Return the archive path associated with this checkpoint attempt.
    #[must_use]
    pub fn path(&self) -> &Path {
        match self {
            Self::Persisted(path) | Self::Throttled(path) | Self::Disabled(path) => path,
        }
    }

    /// Return whether this outcome guarantees that the current snapshot was
    /// written to the archive.
    #[must_use]
    pub fn is_persisted(&self) -> bool {
        matches!(self, Self::Persisted(_))
    }
}

async fn resolve_sessions_dir() -> Result<PathBuf> {
    if let Some(custom) = read_env_var_os(SESSION_DIR_ENV) {
        let path = PathBuf::from(custom);
        VtCodePaths::ensure_user_dir(&path)
            .with_context(|| format!("failed to initialize session directory {}", path.display()))?;
        return Ok(path);
    }

    let paths = VtCodePaths::resolve().context("failed to resolve VT Code paths")?;
    paths
        .ensure_state_dir()
        .context("failed to initialize VT Code state directory")?;
    let dir = paths.state_path("sessions")?;
    VtCodePaths::ensure_user_dir(&dir)
        .with_context(|| format!("failed to initialize session directory {}", dir.display()))?;
    Ok(dir)
}

#[cfg(test)]
#[path = "session_archive_tests.rs"]
mod tests;
