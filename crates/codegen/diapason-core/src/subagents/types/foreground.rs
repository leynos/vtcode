//! Foreground subagent run state: child records, controller state and outcomes.
//!
//! A delegated child's record is runtime state, not a projection or a tool
//! contract, so it lives apart from the public status types.
use chrono::{DateTime, Utc};
use diapason_config::{DiscoveredSubagents, SubagentSpec};
use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use super::ledger_view::BackgroundRecord;
use super::status::{SubagentStatus, SubagentStatusEntry};
use crate::config::DiapasonConfig;
use crate::core::agent::task::TaskOutcome;
use crate::core::threads::ThreadRuntimeHandle;
use crate::llm::provider::Message;
use crate::utils::session_archive::SessionArchiveMetadata;

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
    pub(crate) effective_config: Option<DiapasonConfig>,
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
    pub(crate) child_controller: Option<Arc<super::super::SubagentController>>,
    /// Whether the user explicitly closed this child.
    ///
    /// [`SubagentStatus::Closed`] cannot carry this by itself: `close_single`
    /// deliberately keeps a settled `Completed` or `Failed` outcome visible,
    /// because a transport projects `Closed` as a failure and a user closure
    /// must not turn a successful task into a visible failure. The closure is
    /// therefore recorded beside the outcome, so `send_input` still refuses a
    /// task the user closed and `reopen_single` releases it again. Every access
    /// already holds the controller state lock, so the record is the one
    /// authority for it.
    pub(crate) closed: bool,
}

pub(crate) struct ChildRunRequest {
    pub(crate) prompt: String,
    pub(crate) max_turns: Option<usize>,
    pub(crate) model_override: Option<String>,
    pub(crate) reasoning_override: Option<String>,
}

// ─── Status Entry Builders ──────────────────────────────────────────────────

pub trait StatusEntryBuilder {
    type Entry;
    fn build_status_entry(&self) -> Self::Entry;
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
