//! Session-scoped subagent projections that carry live thread data.
//!
//! An invocation context travels with one parent turn, and a thread snapshot
//! carries the child's live runtime view, so neither belongs with the plain
//! wire DTOs.
use chrono::{DateTime, Utc};
use std::path::PathBuf;

use super::status::SubagentStatus;
use crate::config::DiapasonConfig;
use crate::core::threads::ThreadSnapshot;
use crate::exec::events::ThreadEvent;
use crate::llm::provider::Message;

/// Session-specific parent data for one model tool-execution task.
///
/// Host sessions can share a subagent controller, so this snapshot must travel
/// with the invocation rather than being stored as mutable controller state.
#[derive(Clone)]
pub struct SubagentInvocationContext {
    pub(crate) parent_session_id: String,
    pub(crate) parent_messages: Vec<Message>,
    pub(crate) current_input: String,
}

impl SubagentInvocationContext {
    /// Creates an invocation context captured from one parent session turn.
    #[must_use]
    pub fn new(parent_session_id: String, parent_messages: Vec<Message>, current_input: String) -> Self {
        Self { parent_session_id, parent_messages, current_input }
    }
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
    pub effective_config: DiapasonConfig,
    pub snapshot: ThreadSnapshot,
    pub recent_events: Vec<ThreadEvent>,
}
