//! Persisted session messages, snapshots, progress, and listing types.
use super::SessionArchiveMetadata;
use crate::core::agent::snapshots::SnapshotTurnDiagnostics;
use crate::llm::provider::{AssistantPhase, Message, MessageClearAt, MessageContent, MessageRole, ToolCall};
use crate::utils::error_log_collector::ErrorLogEntry;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use std::path::{Path, PathBuf};

/// A conversation message stored in a session archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SessionMessage {
    /// Role that produced or receives this message.
    pub role: MessageRole,
    /// Text or structured content persisted for the message.
    pub content: MessageContent,
    // Keep sparse metadata boxed so long histories don't pay large inline Option<T> costs.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_string_opt"
    )]
    /// Optional model reasoning associated with the message.
    pub reasoning: Option<Box<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_vec_opt"
    )]
    /// Provider-specific structured reasoning details, when available.
    pub reasoning_details: Option<Box<Vec<serde_json::Value>>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_vec_opt"
    )]
    /// Tool calls requested by this message, if any.
    pub tool_calls: Option<Box<Vec<ToolCall>>>,
    /// Identifier of the tool call answered by this message, if any.
    #[serde(default, deserialize_with = "deserialize_boxed_non_empty_string_opt")]
    pub tool_call_id: Option<Box<String>>,
    /// Assistant response phase associated with the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AssistantPhase>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_non_empty_string_opt"
    )]
    /// Name of the tool that originated this message, if applicable.
    pub origin_tool: Option<Box<String>>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_boxed_metadata_opt"
    )]
    /// Typed metadata attached to the message.
    pub metadata: Option<Box<crate::core::message_metadata::MessageMetadata>>,
    /// Provider-specific clearing semantics for this message, when present.
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

    /// Create a text message with no optional tool or provider metadata.
    pub fn new(role: MessageRole, content: impl Into<String>) -> Self {
        Self::base(role, MessageContent::Text(content.into()))
    }

    /// Create a message with structured or text content and no optional metadata.
    pub fn with_content(role: MessageRole, content: MessageContent) -> Self {
        Self::base(role, content)
    }

    /// Create a text message associated with an optional tool-call identifier.
    pub fn with_tool_call_id(role: MessageRole, content: impl Into<String>, tool_call_id: Option<String>) -> Self {
        Self::with_tool_call_id_content(role, MessageContent::Text(content.into()), tool_call_id)
    }

    /// Create a message with arbitrary content and an optional tool-call identifier.
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

/// Durable snapshot of a completed or in-progress session archive.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionSnapshot {
    /// Workspace, model, provider, and continuation metadata for this session.
    pub metadata: SessionArchiveMetadata,
    /// Time the session archive was created.
    pub started_at: DateTime<Utc>,
    /// Time represented by the latest snapshot write.
    pub ended_at: DateTime<Utc>,
    /// Total number of messages in the session at snapshot time.
    pub total_messages: usize,
    /// Distinct tool names used during the session.
    pub distinct_tools: Vec<String>,
    /// Human-readable transcript lines included in the snapshot.
    pub transcript: Vec<String>,
    /// Persisted conversation messages.
    #[serde(default)]
    pub messages: Vec<SessionMessage>,
    // SessionProgress is heavy and frequently absent in final snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Detailed state from an in-progress checkpoint, when retained.
    pub progress: Option<Box<SessionProgress>>,
    /// ERROR-level log entries captured during the session for post-mortem debugging.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub error_logs: Vec<ErrorLogEntry>,
}

/// Detailed conversation state stored in a progress checkpoint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionProgress {
    /// Turn number represented by this checkpoint.
    pub turn_number: usize,
    /// Recent messages used to build the progress transcript.
    #[serde(default)]
    pub recent_messages: Vec<SessionMessage>,
    /// Compact summaries of tools used by the checkpointed session.
    #[serde(default)]
    pub tool_summaries: Vec<String>,
    /// Token usage summary, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<String>,
    /// Maximum context size associated with the checkpoint, when known.
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

/// A loaded snapshot paired with the archive file from which it was read.
#[derive(Debug, Clone)]
pub struct SessionListing {
    /// Path to the session archive file.
    pub path: PathBuf,
    /// Snapshot decoded from the archive file.
    pub snapshot: SessionSnapshot,
}

/// An archive identifier derived from a loaded session listing.
///
/// The inner value is crate-private so callers cannot mark an arbitrary
/// runtime/session ID as resumable. Obtain one from
/// [`crate::utils::session_archive::SessionArchive::verify_persisted_resume_identifier`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedSessionArchiveIdentifier(pub(crate) String);

impl VerifiedSessionArchiveIdentifier {
    /// Borrow the verified archive identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Inputs used to construct and persist a session progress checkpoint.
#[derive(Debug, Clone)]
pub struct SessionProgressArgs {
    /// Total number of messages in the session.
    pub total_messages: usize,
    /// Tool names to include in the checkpoint summary.
    pub distinct_tools: Vec<String>,
    /// Full message history to persist in the snapshot.
    pub messages: Vec<SessionMessage>,
    /// Recent messages used for the compact progress transcript.
    pub recent_messages: Vec<SessionMessage>,
    /// Current turn number.
    pub turn_number: usize,
    /// Token usage summary, when available.
    pub token_usage: Option<String>,
    /// Maximum context size, when known.
    pub max_context_tokens: Option<usize>,
    /// Skill names loaded for the session, when available.
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
