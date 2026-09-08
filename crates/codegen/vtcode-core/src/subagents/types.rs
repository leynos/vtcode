use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use vtcode_config::{DiscoveredSubagents, SubagentSpec};

use crate::config::VTCodeConfig;
use crate::core::agent::task::TaskOutcome;
use crate::core::threads::{ThreadRuntimeHandle, ThreadSnapshot};
use crate::exec::events::ThreadEvent;
use crate::llm::provider::Message;
use crate::utils::session_archive::SessionArchiveMetadata;

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
pub struct BackgroundSubprocessSnapshot {
    pub entry: BackgroundSubprocessEntry,
    #[serde(default)]
    pub preview: String,
}

#[derive(Debug, Clone)]
pub struct SubagentThreadSnapshot {
    pub id: String,
    pub session_id: String,
    pub parent_thread_id: String,
    pub agent_name: String,
    pub display_label: String,
    pub status: SubagentStatus,
    pub background: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub archive_path: Option<PathBuf>,
    pub transcript_path: Option<PathBuf>,
    pub effective_config: VTCodeConfig,
    pub snapshot: ThreadSnapshot,
    pub recent_events: Vec<ThreadEvent>,
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

// ─── Internal Records ───────────────────────────────────────────────────────

pub struct ChildRecord {
    pub(crate) id: String,
    pub(crate) session_id: String,
    pub(crate) parent_thread_id: String,
    pub(crate) spec: SubagentSpec,
    pub(crate) display_label: String,
    pub(crate) status: SubagentStatus,
    pub(crate) background: bool,
    pub(crate) depth: usize,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) completed_at: Option<DateTime<Utc>>,
    pub(crate) summary: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) archive_metadata: Option<SessionArchiveMetadata>,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) transcript_path: Option<PathBuf>,
    pub(crate) effective_config: Option<VTCodeConfig>,
    pub(crate) stored_messages: Vec<Message>,
    pub(crate) last_prompt: Option<String>,
    pub(crate) queued_prompts: VecDeque<String>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
    pub(crate) thread_handle: Option<ThreadRuntimeHandle>,
    pub(crate) handle: Option<JoinHandle<()>>,
    pub(crate) notify: Arc<Notify>,
    /// Optional worktree path for isolated subagents. When set, the child
    /// agent runs in this worktree instead of the parent workspace root.
    pub(crate) worktree_path: Option<PathBuf>,
    /// Child-scoped subagent controller used to enable nested delegation.
    /// Created once on the first nested-capable run and reused across resumes
    /// so grandchildren spawned by this child remain reachable for the child's
    /// lifetime.
    pub(crate) child_controller: Option<Arc<super::SubagentController>>,
}

pub(crate) struct ChildRunRequest {
    pub(crate) prompt: String,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedBackgroundRecord {
    pub(crate) id: String,
    agent_name: String,
    display_label: String,
    description: String,
    source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour")]
    colour: Option<String>,
    session_id: String,
    exec_session_id: String,
    desired_enabled: bool,
    status: BackgroundSubprocessStatus,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    ended_at: Option<DateTime<Utc>>,
    pid: Option<u32>,
    prompt: String,
    summary: Option<String>,
    error: Option<String>,
    archive_path: Option<PathBuf>,
    transcript_path: Option<PathBuf>,
    max_turns: Option<usize>,
    model_override: Option<String>,
    reasoning_override: Option<String>,
    restart_attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PersistedBackgroundState {
    #[serde(default)]
    pub(crate) records: Vec<PersistedBackgroundRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackgroundRecord {
    pub(crate) id: String,
    pub(crate) agent_name: String,
    pub(crate) display_label: String,
    pub(crate) description: String,
    pub(crate) source: String,
    /// The serialized key `color` is fixed by the schema; `colour` is accepted as a British spelling alias.
    #[serde(rename = "color", alias = "colour")]
    pub(crate) colour: Option<String>,
    pub(crate) session_id: String,
    pub(crate) exec_session_id: String,
    pub(crate) desired_enabled: bool,
    pub(crate) status: BackgroundSubprocessStatus,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) ended_at: Option<DateTime<Utc>>,
    pub(crate) pid: Option<u32>,
    pub(crate) prompt: String,
    pub(crate) summary: Option<String>,
    pub(crate) error: Option<String>,
    pub(crate) archive_path: Option<PathBuf>,
    pub(crate) transcript_path: Option<PathBuf>,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
    pub(crate) restart_attempts: u8,
}

// ─── Status Entry Builders ──────────────────────────────────────────────────

pub trait StatusEntryBuilder {
    type Entry;
    fn build_status_entry(&self) -> Self::Entry;
}

impl StatusEntryBuilder for BackgroundRecord {
    type Entry = BackgroundSubprocessEntry;

    fn build_status_entry(&self) -> BackgroundSubprocessEntry {
        BackgroundSubprocessEntry {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            exec_session_id: self.exec_session_id.clone(),
            agent_name: self.agent_name.clone(),
            display_label: self.display_label.clone(),
            description: self.description.clone(),
            source: self.source.clone(),
            colour: self.colour.clone(),
            status: self.status,
            desired_enabled: self.desired_enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            pid: self.pid,
            summary: self.summary.clone(),
            error: self.error.clone(),
            archive_path: self.archive_path.clone(),
            transcript_path: self.transcript_path.clone(),
        }
    }
}

impl StatusEntryBuilder for ChildRecord {
    type Entry = SubagentStatusEntry;

    fn build_status_entry(&self) -> SubagentStatusEntry {
        SubagentStatusEntry {
            id: self.id.clone(),
            session_id: self.session_id.clone(),
            parent_thread_id: self.parent_thread_id.clone(),
            agent_name: self.spec.name.clone(),
            display_label: self.display_label.clone(),
            description: self.spec.description.clone(),
            source: self.spec.source.label(),
            colour: self.spec.colour.clone(),
            status: self.status,
            background: self.background,
            depth: self.depth,
            created_at: self.created_at,
            updated_at: self.updated_at,
            completed_at: self.completed_at,
            summary: self.summary.clone(),
            error: self.error.clone(),
            transcript_path: self.transcript_path.clone(),
            nickname: self.spec.nickname_candidates.first().cloned(),
        }
    }
}

impl ChildRecord {
    pub(crate) fn dequeue_run(&mut self) -> Option<ChildRunRequest> {
        let prompt = self.queued_prompts.pop_front()?;
        self.status = SubagentStatus::Running;
        self.updated_at = Utc::now();
        Some(ChildRunRequest {
            prompt,
            max_turns: self.max_turns,
            model_override: self.model_override.clone(),
            reasoning_override: self.reasoning_override.clone(),
        })
    }

    /// Apply execution result to the record state. Returns whether the child
    /// still has queued work and should continue looping.
    pub(crate) fn apply_result(&mut self, execute: anyhow::Result<ChildRunResult>) -> bool {
        match execute {
            Ok(result) => {
                self.status = if result.outcome.is_success() {
                    SubagentStatus::Completed
                } else {
                    SubagentStatus::Failed
                };
                self.summary = Some(result.summary);
                // Store error info for all non-success outcomes, not just Failed.
                // This ensures budget/turn/tool-loop limits and other terminations
                // carry a diagnostic message for debugging.
                self.error = if result.outcome.is_success() {
                    None
                } else {
                    Some(result.outcome.description())
                };
                self.transcript_path = result.transcript_path;
                self.stored_messages = result.messages;
            }
            Err(error) => {
                self.status = SubagentStatus::Failed;
                self.summary = None;
                self.error = Some(error.to_string());
            }
        }
        let has_more_work = !self.queued_prompts.is_empty();
        if has_more_work {
            self.status = SubagentStatus::Queued;
            self.completed_at = None;
        } else if self.status.is_terminal() {
            self.completed_at = Some(Utc::now());
        }
        self.notify.notify_waiters();
        has_more_work
    }

    /// Build the hook payload for a terminal run (no more queued prompts).
    pub(crate) fn build_hook_payload(&self) -> (String, String, String, String, bool, String, Option<PathBuf>) {
        (
            self.parent_thread_id.clone(),
            self.session_id.clone(),
            self.spec.name.clone(),
            self.display_label.clone(),
            self.background,
            self.status.as_str().to_string(),
            self.transcript_path.clone().or_else(|| self.archive_path.clone()),
        )
    }
}

impl BackgroundRecord {
    pub(crate) fn into_persisted(self) -> PersistedBackgroundRecord {
        PersistedBackgroundRecord {
            id: self.id,
            agent_name: self.agent_name,
            display_label: self.display_label,
            description: self.description,
            source: self.source,
            colour: self.colour,
            session_id: self.session_id,
            exec_session_id: self.exec_session_id,
            desired_enabled: self.desired_enabled,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
            started_at: self.started_at,
            ended_at: self.ended_at,
            pid: self.pid,
            prompt: self.prompt,
            summary: self.summary,
            error: self.error,
            archive_path: self.archive_path,
            transcript_path: self.transcript_path,
            max_turns: self.max_turns,
            model_override: self.model_override,
            reasoning_override: self.reasoning_override,
            restart_attempts: self.restart_attempts,
        }
    }

    pub(crate) fn from_persisted(record: PersistedBackgroundRecord) -> Self {
        Self {
            id: record.id,
            agent_name: record.agent_name,
            display_label: record.display_label,
            description: record.description,
            source: record.source,
            colour: record.colour,
            session_id: record.session_id,
            exec_session_id: record.exec_session_id,
            desired_enabled: record.desired_enabled,
            status: record.status,
            created_at: record.created_at,
            updated_at: record.updated_at,
            started_at: record.started_at,
            ended_at: record.ended_at,
            pid: record.pid,
            prompt: record.prompt,
            summary: record.summary,
            error: record.error,
            archive_path: record.archive_path,
            transcript_path: record.transcript_path,
            max_turns: record.max_turns,
            model_override: record.model_override,
            reasoning_override: record.reasoning_override,
            restart_attempts: record.restart_attempts,
        }
    }
}

// ─── Controller State ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Default)]
pub struct TurnDelegationHints {
    pub(crate) explicit_mentions: Vec<String>,
    pub(crate) explicit_request: bool,
    pub(crate) current_input: String,
}

pub struct ControllerState {
    pub(crate) discovered: DiscoveredSubagents,
    pub(crate) parent_messages: Vec<Message>,
    pub(crate) turn_hints: TurnDelegationHints,
    pub(crate) children: BTreeMap<String, ChildRecord>,
    pub(crate) background_children: BTreeMap<String, BackgroundRecord>,
}

// ─── Child Run Result ───────────────────────────────────────────────────────

pub struct ChildRunResult {
    pub(crate) messages: Vec<Message>,
    pub(crate) summary: String,
    pub(crate) outcome: TaskOutcome,
    pub(crate) transcript_path: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    //! Verifies stable colour aliases for subagent status and persistence wire records.

    use super::*;
    use chrono::{DateTime, Utc};
    use serde_json::json;

    #[test]
    fn subagent_status_entry_accepts_colour_and_serializes_color() {
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
        assert_eq!(canonical.get("color"), Some(&json!("orchid")), "status output should use the canonical color key");
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized status entry should be an object")
            .remove("color")
            .expect("the canonical color key should be present");
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

    #[test]
    fn background_subprocess_entry_accepts_colour_and_serializes_color() {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        let entry = BackgroundSubprocessEntry {
            id: "background-1".to_string(),
            session_id: "session-1".to_string(),
            exec_session_id: "exec-1".to_string(),
            agent_name: "colour-agent".to_string(),
            display_label: "Colour agent".to_string(),
            description: "A background wire fixture".to_string(),
            source: "project".to_string(),
            colour: Some("orchid".to_string()),
            status: BackgroundSubprocessStatus::Running,
            desired_enabled: true,
            created_at: now,
            updated_at: now,
            started_at: Some(now),
            ended_at: None,
            pid: None,
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
        };

        let mut canonical = serde_json::to_value(&entry).expect("the background entry should serialize");
        assert_eq!(
            canonical.get("color"),
            Some(&json!("orchid")),
            "background output should use the canonical color key"
        );
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized background entry should be an object")
            .remove("color")
            .expect("the canonical color key should be present");
        let previous_british_alias = canonical
            .as_object_mut()
            .expect("a serialized background entry should remain an object")
            .insert("colour".to_string(), canonical_value);
        assert!(previous_british_alias.is_none(), "the British alias should be absent before deserialization");

        let british: BackgroundSubprocessEntry =
            serde_json::from_value(canonical).expect("the British background-entry alias should deserialize");
        assert_eq!(
            british.colour.as_deref(),
            Some("orchid"),
            "the British alias should preserve the background colour"
        );
    }

    #[test]
    fn persisted_background_record_accepts_colour_and_serializes_color() {
        let now = DateTime::<Utc>::UNIX_EPOCH;
        let record = PersistedBackgroundRecord {
            id: "background-1".to_string(),
            agent_name: "colour-agent".to_string(),
            display_label: "Colour agent".to_string(),
            description: "A persistence wire fixture".to_string(),
            source: "project".to_string(),
            colour: Some("orchid".to_string()),
            session_id: "session-1".to_string(),
            exec_session_id: "exec-1".to_string(),
            desired_enabled: true,
            status: BackgroundSubprocessStatus::Running,
            created_at: now,
            updated_at: now,
            started_at: Some(now),
            ended_at: None,
            pid: None,
            prompt: "resume".to_string(),
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
        };

        let mut canonical = serde_json::to_value(&record).expect("the persisted record should serialize");
        assert_eq!(
            canonical.get("color"),
            Some(&json!("orchid")),
            "persisted state should use the canonical color key"
        );
        let canonical_value = canonical
            .as_object_mut()
            .expect("a serialized persisted record should be an object")
            .remove("color")
            .expect("the canonical color key should be present");
        let previous_british_alias = canonical
            .as_object_mut()
            .expect("a serialized persisted record should remain an object")
            .insert("colour".to_string(), canonical_value);
        assert!(previous_british_alias.is_none(), "the British alias should be absent before deserialization");

        let british: PersistedBackgroundRecord =
            serde_json::from_value(canonical).expect("the British persisted-record alias should deserialize");
        assert_eq!(british.colour.as_deref(), Some("orchid"), "the British alias should preserve persisted colour");
    }
}
