//! Coordinates subagent discovery, execution, background work, and the public controller API.
//!
//! Storage access for live and archived background output is injectable through
//! [`BackgroundOutputReader`]. A reader is keyed by the live record of one
//! background task and returns a domain result, so the controller keeps line
//! bounds, transport limits, validation, and error mapping.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
// ─── Module Structure ───────────────────────────────────────────────────────

mod background;
mod background_output;
mod config;
mod constants;
mod controller_child_ids;
mod controller_persistence;
mod discovery;
mod model;
mod prompt;
mod types;

// ─── Re-exports ─────────────────────────────────────────────────────────────

pub use background::{
    background_record_id, background_record_id_for_owner, build_background_subagent_command, extract_tail_lines,
    load_archive_preview, subagent_display_label,
};
pub use background_output::{BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, ExecSessionOutputReader};
pub use config::{
    ResolvedAgentRuntimeView, build_child_config, compose_subagent_instructions, filter_child_tools,
    normalize_background_child_max_turns, normalize_child_max_turns, prepare_child_runtime_config,
};
pub use model::{
    agent_type_for_spec, load_memory_appendix, load_memory_appendix_async, load_primary_memory_appendix,
    load_primary_memory_appendix_async,
};
pub use prompt::{
    contains_explicit_delegation_request, contains_explicit_model_request, delegated_task_requires_clarification,
    extract_explicit_agent_mentions, normalize_requested_model_override, request_prompt, sanitize_subagent_input_items,
};
pub use types::{
    BackgroundRecord, BackgroundSubprocessEntry, BackgroundSubprocessSnapshot, BackgroundSubprocessStatus, ChildRecord,
    ChildRunResult, ControllerState, PersistedBackgroundRecord, PersistedBackgroundState, SendInputRequest,
    SpawnAgentRequest, SpawnBackgroundSubprocessRequest, StatusEntryBuilder, SubagentInputItem,
    SubagentInvocationContext, SubagentProgressObserver, SubagentStatus, SubagentStatusEntry, SubagentThreadSnapshot,
    TurnDelegationHints,
};

// VerificationResult is defined in this module (below) and re-exported at the
// crate root via `pub use subagents::VerificationResult`.

// ─── Public Utilities ───────────────────────────────────────────────────────

/// Returns `true` if `name` is one of the reserved subagent-internal tool names.
pub fn is_subagent_tool(name: &str) -> bool {
    SUBAGENT_TOOL_NAMES.contains(&name)
}

#[derive(Clone, Default)]
pub(super) struct BackgroundLaunchOverrides {
    prompt: Option<String>,
    max_turns: Option<usize>,
    model_override: Option<String>,
    reasoning_override: Option<String>,
    parent_session_id: Option<String>,
}

#[derive(Clone, Default)]
pub(super) struct PreparedDelegationContext {
    requested_agent: Option<String>,
    explicit_mentions: Vec<String>,
    explicit_request: bool,
    parent_session_id: String,
    parent_messages: Vec<Message>,
}

/// Result of a propose/verify cycle.
///
/// Returned by [`SubagentController::verify_proposed_change`]. The caller
/// inspects `approved` to decide whether to commit or retry the mutation.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    /// Whether the verifier approved the change.
    pub approved: bool,
    /// Concrete issues identified by the verifier (empty if approved).
    pub issues: Vec<String>,
    /// Free-text reasoning from the verifier.
    pub reasoning: String,
}

// ─── Controller ─────────────────────────────────────────────────────────────

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures::future::select_all;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, Weak};
use tokio::sync::{Notify, RwLock};

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, find_session_by_identifier};
use diapason_config::SubagentSpec;
use diapason_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use diapason_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

tokio::task_local! {
    static SUBAGENT_INVOCATION_CONTEXT: SubagentInvocationContext;
}

/// Runs one local tool invocation with parent-session context captured by its adapter.
pub async fn with_subagent_invocation_context<F: Future>(context: SubagentInvocationContext, future: F) -> F::Output {
    SUBAGENT_INVOCATION_CONTEXT.scope(context, future).await
}

fn current_subagent_invocation_context() -> Option<SubagentInvocationContext> {
    SUBAGENT_INVOCATION_CONTEXT.try_with(Clone::clone).ok()
}

// ─── Controller Config ─────────────────────────────────────────────────────

/// Configuration required to construct a [`SubagentController`].
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
    /// Whether this controller manages a background runtime subprocess.
    pub managed_background_runtime: bool,
}

/// Central controller that manages spawning, lifecycle, and state of all subagents.
#[derive(Clone)]
pub struct SubagentController {
    config: Arc<SubagentControllerConfig>,
    /// Reads live and archived background output. Resolved once at construction
    /// so every read path shares the same injectable storage access.
    output_reader: Arc<dyn BackgroundOutputReader>,
    background_owner_session_id: Option<String>,
    parent_session_id: Arc<RwLock<String>>,
    lifecycle_hooks: Option<LifecycleHookEngine>,
    state: Arc<RwLock<ControllerState>>,
    shutdown_requested: Arc<AtomicBool>,
    /// Transient close-in-progress flag. Unlike `shutdown_requested` this is
    /// cleared when a subtree is reopened via `reopen_single`, so a resumed
    /// child can delegate again and its controller keeps saving background
    /// state. Set only while `close_tree`/`signal_shutdown` are tearing a
    /// subtree down.
    closing: Arc<AtomicBool>,
    progress_observers: Arc<controller_observers::ProgressObserverRegistry>,
    /// Observer key inherited from the owning controller. Child-scoped
    /// controllers serve exactly one parent session, so their lifecycle
    /// events are published under that session's observer key even though the
    /// tasks' immediate `parent_thread_id` is the child's own session.
    progress_observer_scope: Option<String>,
    /// Child task ids allocated anywhere in this controller's tree. A
    /// controller-local check cannot see an id a sibling allocated, so the
    /// root owns one scope and the whole tree shares one slot pointing at it.
    /// A child adopts its parent's slot object, so the slot is also the
    /// redirectable handle that attaching a controller with existing children
    /// rewrites: every descendant follows the redirect at once.
    child_id_scope: Arc<std::sync::RwLock<Arc<controller_spawn_run::ChildIdScopeSlot>>>,
}

impl SubagentController {
    /// Creates a new controller, discovering subagent specs and loading persisted background state.
    pub fn new(config: SubagentControllerConfig) -> impl Future<Output = Result<Self>> + Send {
        Self::new_with_background_owner(config, None)
    }

    /// Creates a controller scoped to one persisted background owner.
    ///
    /// Interactive sessions use this constructor so a per-session
    /// controller cannot load or restore another session's background records.
    /// Callers such as the CLI use [`Self::new`] to retain restore-all
    /// behaviour.
    pub fn new_with_background_owner(
        config: SubagentControllerConfig,
        owner_session_id: Option<&str>,
    ) -> impl Future<Output = Result<Self>> + Send {
        let background_owner_session_id = owner_session_id.map(str::to_owned);
        Box::pin(async move {
            let discovered = discover_controller_subagents(&config.workspace_root).await?;
            let workspace_gated = config.workspace_gated;
            let lifecycle_hooks = LifecycleHookEngine::new_with_session_gated(
                config.workspace_root.clone(),
                &config.vt_cfg.hooks,
                SessionStartTrigger::Startup,
                config.parent_session_id.clone(),
                workspace_gated,
            )?;
            if let Some(engine) = lifecycle_hooks.as_ref() {
                crate::hooks::lifecycle::restore_workspace_hook_approval(engine, &config.workspace_root).await;
            }
            // A nested scope never owns background work:
            // `ensure_background_subprocess_enabled` refuses to launch one.
            // Loading the workspace ledger here would give the nested scope a
            // second copy of records another controller already owns, so a
            // list/cancel routed to either scope could report a stale copy
            // (for example `Running` after the owner cancelled it).
            let background_children = if config.managed_background_runtime {
                BTreeMap::new()
            } else {
                // A legacy ledger can hold two ids that canonicalize to the
                // same key, so several entries can name one record. The load
                // path and the save merge share `collect_by_canonical_id`, so
                // the newest `updated_at` wins in both: file order is not a
                // freshness signal, and letting it decide could load a
                // cancelled copy over the cancellation.
                controller_persistence::collect_by_canonical_id(
                    load_background_state(&config.workspace_root)
                        .await?
                        .records
                        .into_iter()
                        .filter(|record| {
                            background_owner_session_id
                                .as_deref()
                                .is_none_or(|owner| record.owner_session_id.as_deref() == Some(owner))
                        }),
                )
                .into_iter()
                .map(|(id, record)| (id, BackgroundRecord::from_persisted(record)))
                .collect()
            };
            let parent_session_id = Arc::new(RwLock::new(config.parent_session_id.clone()));
            let output_reader = config
                .output_reader
                .clone()
                .unwrap_or_else(|| Arc::new(ExecSessionOutputReader::new(config.exec_sessions.clone())));
            Ok(Self {
                config: Arc::new(config),
                output_reader,
                background_owner_session_id,
                parent_session_id,
                lifecycle_hooks,
                state: Arc::new(RwLock::new(ControllerState {
                    discovered,
                    parent_messages: Vec::new(),
                    turn_hints: TurnDelegationHints::default(),
                    children: BTreeMap::new(),
                    background_children,
                })),
                shutdown_requested: Arc::new(AtomicBool::new(false)),
                closing: Arc::new(AtomicBool::new(false)),
                progress_observers: Arc::default(),
                progress_observer_scope: None,
                child_id_scope: Arc::new(std::sync::RwLock::new(Arc::default())),
            })
        })
    }

    /// Re-discovers subagent specs from the workspace.
    pub async fn reload(&self) -> Result<()> {
        let discovered = discover_controller_subagents(&self.config.workspace_root).await?;
        self.state.write().await.discovered = discovered;
        Ok(())
    }

    /// Stores the parent conversation messages for context forking into children.
    pub async fn set_parent_messages(&self, messages: &[Message]) {
        let cloned = messages.to_vec();
        self.state.write().await.parent_messages = cloned;
    }

    /// Parses the current user input to extract explicit agent mentions and delegation signals.
    pub async fn set_turn_delegation_hints_from_input(&self, input: &str) -> Vec<String> {
        let mut state = self.state.write().await;
        let explicit_mentions = extract_explicit_agent_mentions(input, state.discovered.effective.as_slice());
        let explicit_request = contains_explicit_delegation_request(input, explicit_mentions.as_slice());
        state.turn_hints = TurnDelegationHints {
            explicit_mentions: explicit_mentions.clone(),
            explicit_request,
            current_input: input.to_string(),
        };
        explicit_mentions
    }

    /// Resets delegation hints at the end of a turn.
    pub async fn clear_turn_delegation_hints(&self) {
        self.state.write().await.turn_hints = TurnDelegationHints::default();
    }

    /// Updates the parent session identifier at runtime.
    pub async fn set_parent_session_id(&self, session_id: impl Into<String>) {
        *self.parent_session_id.write().await = session_id.into();
    }

    /// Returns the currently effective subagent specifications (merged builtin + workspace).
    pub async fn effective_specs(&self) -> Vec<SubagentSpec> {
        self.state.read().await.discovered.effective.clone()
    }

    /// Returns specs that are shadowed by workspace-level overrides.
    pub async fn shadowed_specs(&self) -> Vec<SubagentSpec> {
        self.state.read().await.discovered.shadowed.clone()
    }

    /// Returns status entries for all tracked child subagents.
    pub async fn status_entries(&self) -> Vec<SubagentStatusEntry> {
        let state = self.state.read().await;
        let invocation_owner = current_subagent_invocation_context().map(|context| context.parent_session_id);
        state
            .children
            .values()
            .filter(|record| invocation_owner.as_ref().is_none_or(|owner| &record.parent_thread_id == owner))
            .map(ChildRecord::build_status_entry)
            .collect()
    }
}

// ─── Controller submodule split ────────────────────────────────────────────

mod controller_background_ops;
mod controller_child_loop;
mod controller_helpers;
mod controller_observers;
mod controller_spawn_run;
mod controller_tree;
mod controller_verify;

pub use controller_observers::SubagentProgressObserverRegistration;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub(super) use controller_helpers::*;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
