//! Public status, liveness and tool-contract types for subagents.
//!
//! These are the shapes adapters project to their own delivery protocol and
//! provider tools accept, so they stay separate from the controller's
//! foreground run state. Serialized names are part of the wire contract.
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ─── Public Status Types ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SubagentStatus {
    Queued,
    Running,
    Waiting,
    Completed,
    Failed,
    Closed,
}

impl SubagentStatus {
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Closed)
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundSubprocessStatus {
    Starting,
    Running,
    Stopped,
    Error,
}

impl BackgroundSubprocessStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Error => "error",
        }
    }

    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}

// ─── Public DTOs ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentStatusEntry {
    pub id: String,
    pub session_id: String,
    pub parent_thread_id: String,
    pub agent_name: String,
    pub display_label: String,
    pub description: String,
    pub source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour", default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    pub status: SubagentStatus,
    pub background: bool,
    pub depth: usize,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundSubprocessEntry {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_session_id: Option<String>,
    pub session_id: String,
    pub exec_session_id: String,
    pub agent_name: String,
    pub display_label: String,
    pub description: String,
    pub source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour", default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
    pub status: BackgroundSubprocessStatus,
    pub desired_enabled: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentInputItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpawnAgentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub items: Vec<SubagentInputItem>,
    #[serde(default)]
    pub fork_context: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub background: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpawnBackgroundSubprocessRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub items: Vec<SubagentInputItem>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SendInputRequest {
    #[serde(rename = "id")]
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub items: Vec<SubagentInputItem>,
    #[serde(default)]
    pub interrupt: bool,
}

/// Receives domain status snapshots when a delegated child or managed
/// background process changes state.
///
/// Adapters implement this port to project status into their own delivery
/// protocol. The snapshots remain domain data; this trait does not prescribe
/// transport, serialization, or buffering.
pub trait SubagentProgressObserver: Send + Sync {
    /// Receives an updated delegated-child status snapshot.
    fn subagent_status_changed(&self, entry: SubagentStatusEntry);

    /// Receives an updated managed-background-process status snapshot.
    fn background_status_changed(&self, entry: BackgroundSubprocessEntry);

    /// Receives a delegated-child snapshot with the revision that orders it.
    ///
    /// The controller assigns `lifecycle_revision` under the state lock that
    /// captured `entry`, so two publications of one task are ordered exactly
    /// as the states they observed: a capture taken before a later state
    /// change keeps the lower revision even when it is delivered after the
    /// newer capture. An adapter that buffers snapshots must compare this
    /// revision instead of `updated_at`, because several transitions can be
    /// recorded within one clock tick.
    ///
    /// The default delivers inline through [`Self::subagent_status_changed`]
    /// for observers that need no ordering.
    fn subagent_status_published(&self, entry: SubagentStatusEntry, _lifecycle_revision: u64) {
        self.subagent_status_changed(entry);
    }

    /// Background counterpart of [`Self::subagent_status_published`].
    fn background_status_published(&self, entry: BackgroundSubprocessEntry, _lifecycle_revision: u64) {
        self.background_status_changed(entry);
    }
}

#[cfg(test)]
mod tests {
    //! Verifies stable colour aliases for subagent status wire records.

    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    #[test]
    fn subagent_status_entry_accepts_colour_and_serializes_the_fixed_key() {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        let entry = SubagentStatusEntry {
            id: "child-1".to_string(),
            session_id: "session-1".to_string(),
            parent_thread_id: "thread-1".to_string(),
            agent_name: "colour-agent".to_string(),
            display_label: "Colour agent".to_string(),
            description: "A status wire fixture".to_string(),
            source: "project".to_string(),
            colour: Some("orchid".to_string()),
            status: SubagentStatus::Running,
            background: false,
            depth: 1,
            created_at: now,
            updated_at: now,
            completed_at: None,
            summary: None,
            error: None,
            transcript_path: None,
            nickname: None,
        };

        let mut canonical = serde_json::to_value(&entry).expect("the status entry should serialize");
        assert_eq!(canonical.get("color"), Some(&json!("orchid")), "status output should use the fixed wire key");
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized status entry should be an object")
            .remove("color")
            .expect("the fixed wire key should be present");
        let previous_british_alias = canonical
            .as_object_mut()
            .expect("a serialized status entry should remain an object")
            .insert("colour".to_string(), canonical_value);
        assert!(previous_british_alias.is_none(), "the British alias should be absent before deserialization");

        let british: SubagentStatusEntry =
            serde_json::from_value(canonical.clone()).expect("the British status-entry alias should deserialize");
        assert_eq!(british.colour.as_deref(), Some("orchid"), "the British alias should preserve the status colour");

        let previous_canonical_key = canonical
            .as_object_mut()
            .expect("a serialized status entry should remain an object")
            .insert("color".to_string(), json!("violet"));
        assert!(
            previous_canonical_key.is_none(),
            "the canonical key should be absent before duplicate-alias testing"
        );
        assert!(
            serde_json::from_value::<SubagentStatusEntry>(canonical).is_err(),
            "both status-entry spellings should be rejected as duplicate aliases"
        );
    }
}
