//! Coordinates subagent discovery, execution, background work, and the public controller API.
//!
//! Storage access for live and archived background output is injectable through
//! [`BackgroundOutputReader`]. A reader is keyed by the live record of one
//! background task and returns a domain result, so the controller keeps line
//! bounds, transport limits, validation, and error mapping.
//!
//! Background records are keyed by their canonical, owner-scoped id once a
//! controller has loaded them: the load path rewrites a legacy
//! workspace-global id, so an exact map lookup must use the canonical id.
//! Session resolution is the deliberate exception. A session that still holds
//! an id from before owner scoping resolves it through
//! [`resolve_owned_background_id`]: the exact canonical id wins, and otherwise
//! the record whose pre-owner-scoping id matches is returned, but only when
//! that record is one the resolving session owns. A legacy id therefore
//! addresses the caller's own record and can never widen ownership to another
//! session's. The rewrite remains a breaking change for callers that insist on
//! an exact lookup of a legacy id.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
// ─── Module Structure ───────────────────────────────────────────────────────

mod background;
mod background_ledger;
mod background_lock;
mod background_output;
mod call_action;
mod config;
mod constants;
mod controller_child_ids;
mod controller_config;
mod controller_locks;
mod controller_persistence;
mod discovery;
mod model;
mod ownership;
mod prompt;
mod types;

// ─── Re-exports ─────────────────────────────────────────────────────────────

pub use background::{
    BackgroundLedgerGuard, background_record_id, background_record_id_for_owner, build_background_subagent_command,
    extract_tail_lines, load_archive_preview, lock_background_ledger, subagent_display_label,
};
pub use background_ledger::{PersistedBackgroundRecord, PersistedBackgroundState};
pub use background_output::{
    BackgroundOutput, BackgroundOutputError, BackgroundOutputReader, BackgroundTaskRef, ExecSessionOutputReader,
};
pub(crate) use background_output::{controller_exec_session_id, controller_exec_session_task_id};
pub use call_action::{agent_call_action, agent_call_targets};
pub use config::{
    ResolvedAgentRuntimeView, build_child_config, compose_subagent_instructions, filter_child_tools,
    normalize_background_child_max_turns, normalize_child_max_turns, prepare_child_runtime_config,
};
pub use controller_background_ops::BackgroundStopOutcome;
pub use controller_config::SubagentControllerConfig;
pub use controller_locks::SubagentLockScope;
pub use model::{
    agent_type_for_spec, load_memory_appendix, load_memory_appendix_async, load_primary_memory_appendix,
    load_primary_memory_appendix_async,
};
pub use ownership::{TaskOwner, owned_background_ids, owned_child_ids, resolve_owned_background_id};
pub use prompt::{
    contains_explicit_delegation_request, contains_explicit_model_request, delegated_task_requires_clarification,
    extract_explicit_agent_mentions, normalize_requested_model_override, request_prompt, sanitize_subagent_input_items,
};
pub(crate) use types::HookPayload;
pub use types::{
    BackgroundLiveness, BackgroundLivenessEntry, BackgroundRecord, BackgroundSubprocessEntry,
    BackgroundSubprocessSnapshot, BackgroundSubprocessStatus, ChildRecord, ChildRunResult, ControllerState,
    SendInputRequest, SpawnAgentRequest, SpawnBackgroundSubprocessRequest, StatusEntryBuilder, SubagentInputItem,
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

/// Returns the session that owns the running tool invocation, if there is one.
///
/// A provider tool executes inside the invocation that called it, so it can
/// name the session it acts for without the controller consulting the ambient
/// context itself. Returning an owned id keeps the borrow independent of the
/// task-local scope, which a `TaskOwner<'_>` cannot otherwise outlive.
#[must_use]
pub fn current_subagent_owner_session_id() -> Option<String> {
    current_subagent_invocation_context().map(|context| context.parent_session_id)
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
    /// Where this controller publishes its lifecycle snapshots, and under which
    /// observer key. Interior-mutable so an independently constructed
    /// controller can adopt the route of the tree it is attached to, even
    /// though attachment only holds a shared `Arc` of it.
    progress_route: controller_observers::ProgressObserverRoute,
    /// Child task ids allocated anywhere in this controller's tree. A
    /// controller-local check cannot see an id a sibling allocated, so the
    /// root owns one scope and the whole tree shares one slot pointing at it.
    /// A child adopts its parent's slot object, so the slot is also the
    /// redirectable handle that attaching a controller with existing children
    /// rewrites: every descendant follows the redirect at once.
    child_id_scope: Arc<std::sync::RwLock<Arc<controller_spawn_run::ChildIdScopeSlot>>>,
    /// Adoption and persistence locks shared by this controller tree. Resolved
    /// once at construction from the config, so a child inherits its parent's
    /// tree and an injected scope is observable at every call site.
    lock_scope: Arc<SubagentLockScope>,
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
        // An empty owner names no owner: `background_record_id_for_owner`
        // already maps it to the base id, and a `Some("")` would otherwise
        // filter the ledger as a distinct owner and merge saves under a
        // phantom identity.
        let background_owner_session_id = owner_session_id.filter(|owner| !owner.is_empty()).map(str::to_owned);
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
                //
                // Breaking id change: this rekeys every entry to its
                // canonical owner-scoped id, so the map below is keyed by the
                // canonical id only. A caller that addresses a record by a
                // legacy, pre-canonicalization id (the workspace-global
                // `background-<agent>`) cannot rely on an exact map lookup;
                // the query paths call `resolve_background_target`, which
                // retries the legacy id, so retained ids still address the
                // record they named.
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
            // Resolve the tree's locks before the state is built: the child-id
            // slot needs the same adoption lock as the controller, and a
            // configured scope must keep the two identities together.
            let lock_scope = config.lock_scope.clone().unwrap_or_else(controller_locks::default_lock_scope);
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
                progress_route: Default::default(),
                child_id_scope: Arc::new(std::sync::RwLock::new(Arc::new(
                    controller_spawn_run::ChildIdScopeSlot::with_lock_scope(Arc::clone(&lock_scope)),
                ))),
                lock_scope,
            })
        })
    }

    /// Returns the adoption and persistence locks this controller tree holds.
    pub(crate) fn lock_scope(&self) -> &Arc<SubagentLockScope> {
        &self.lock_scope
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
