//! Construction input for [`SubagentController`](super::SubagentController).
//!
//! The config is kept beside the controller rather than inside it so the
//! controller module stays focused on orchestration, and adapters can build the
//! config without importing the controller's internals.
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::PtyManager;
use diapason_config::auth::OpenAIChatGptAuthHandle;

use super::background_output::BackgroundOutputReader;
use super::controller_locks::SubagentLockScope;

/// Configuration required to construct a [`SubagentController`](super::SubagentController).
#[derive(Clone)]
pub struct SubagentControllerConfig {
    /// Workspace root directory for the session.
    pub workspace_root: PathBuf,
    /// Session identifier of the parent agent.
    pub parent_session_id: String,
    /// Model identifier used by the parent agent.
    pub parent_model: String,
    /// Provider name used by the parent agent.
    pub parent_provider: String,
    /// Reasoning effort level of the parent agent.
    pub parent_reasoning_effort: ReasoningEffortLevel,
    /// API key for LLM provider access.
    pub api_key: String,
    /// Full Diapason configuration.
    pub vt_cfg: DiapasonConfig,
    /// Optional OpenAI ChatGPT authentication handle.
    pub openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
    /// Current nesting depth of the subagent hierarchy.
    pub depth: usize,
    /// Whether the subagent lifecycle engine must be workspace-gated. Mirrors
    /// the main-session rule: pass `true` whenever workspace-controlled hook
    /// content is present (workspace config.toml/.diapason layers OR a primary
    /// agent spec contributing workspace-controlled hooks). Failing to gate
    /// here would let workspace hooks run without user approval in a
    /// subagent context.
    pub workspace_gated: bool,
    /// Manager for exec sessions (PTY and pipe).
    pub exec_sessions: ExecSessionManager,
    /// Reader for live and archived background output; `None` uses the exec-session default.
    pub output_reader: Option<Arc<dyn BackgroundOutputReader>>,
    /// PTY session manager.
    pub pty_manager: PtyManager,
    /// Whether this controller is forbidden to own background work.
    ///
    /// `true` marks a controller that must not own background subprocesses: it
    /// adopts no background work, neither loads nor saves the background
    /// ledger, and it rejects a background launch. The controller that manages
    /// the background runtime owns its background records and persists them to
    /// the workspace ledger; a nested controller inside that runtime inherits
    /// the restriction through this same flag. Set it on the controller that
    /// runs *inside* a managed runtime, not on the controller that manages one.
    pub managed_background_runtime: bool,
    /// Adoption and persistence locks shared by this controller tree.
    ///
    /// `None` resolves to the process-global default, so controllers created
    /// without one still exclude each other exactly as the former static locks
    /// did. Supply a distinct scope to give a tree its own locks for
    /// injection or isolated tests; a child-scoped controller inherits its
    /// parent's scope.
    pub lock_scope: Option<Arc<SubagentLockScope>>,
}
