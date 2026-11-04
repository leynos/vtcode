//! Context management for vibe coding support
//!
//! This module provides intelligent context gathering and entity resolution
//! to support lazy/vague user requests. It enables "vibe coding" by inferring
//! user intent from minimal input.
//!
//! ## Components
//!
//! - `entity_resolver`: Maps vague terms to workspace entities
//! - `workspace_state`: Tracks file activity and value changes
//! - `conversation_memory`: Maintains entity mentions across conversation

pub mod conversation_memory;
pub mod dynamic_init;
pub mod entity_resolver;
pub mod history_files;
pub mod workspace_state;

// Re-export key types for convenience
pub use conversation_memory::{ConversationMemory, EntityMention, MentionHistory};
pub use dynamic_init::{
    DynamicContextDirs, ensure_mcp_dynamic_context, ensure_skills_dynamic_context, initialize_dynamic_context,
};
pub use entity_resolver::{EntityIndex, EntityMatch, EntityResolver, FileLocation};
pub use history_files::{HistoryConfig, HistoryFileManager, HistoryMessage, HistoryWriteResult};
pub use workspace_state::{FileActivity, ValueHistory, WorkspaceState};
