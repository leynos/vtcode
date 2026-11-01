//! Coordinates subagent discovery, execution, background work, and the public controller API.
#![allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
// ─── Module Structure ───────────────────────────────────────────────────────

mod background;
mod config;
mod constants;
mod discovery;
mod model;
mod prompt;
mod types;

// ─── Re-exports ─────────────────────────────────────────────────────────────

pub use background::{
    background_record_id, background_record_id_for_owner, build_background_subagent_command, extract_tail_lines,
    load_archive_preview, normalize_output_tail_lines, subagent_display_label,
};
pub use config::{
    ResolvedAgentRuntimeView, build_child_config, compose_subagent_instructions, filter_child_tools,
    normalize_background_child_max_turns, normalize_child_max_turns, prepare_child_runtime_config,
};
pub use constants::{DEFAULT_SUBAGENT_OUTPUT_TAIL_LINES, MAX_SUBAGENT_OUTPUT_TAIL_LINES};
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

use crate::config::VTCodeConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::core::agent::runner::{AgentRunner, RunnerSettings};
use crate::core::agent::task::Task;
use crate::core::threads::{ThreadBootstrap, ThreadId, ThreadRuntimeHandle, ThreadSnapshot};
use crate::hooks::{LifecycleHookEngine, SessionStartTrigger};
use crate::llm::provider::Message;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::pty::{PtyManager, PtySize};
use crate::utils::session_archive::{SessionArchive, find_session_by_identifier};
use vtcode_config::SubagentSpec;
use vtcode_config::auth::OpenAIChatGptAuthHandle;

use self::background::*;
use self::config::*;
use self::constants::*;
use self::discovery::discover_controller_subagents;
use self::model::*;
use vtcode_config::subagents::SUBAGENT_HARD_CONCURRENCY_LIMIT;

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
    /// Full VT Code configuration.
    pub vt_cfg: VTCodeConfig,
    /// Optional OpenAI ChatGPT authentication handle.
    pub openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
    /// Current nesting depth of the subagent hierarchy.
    pub depth: usize,
    /// Whether the subagent lifecycle engine must be workspace-gated. Mirrors
    /// the main-session rule: pass `true` whenever workspace-controlled hook
    /// content is present (workspace vtcode.toml/.vtcode layers OR a primary
    /// agent spec contributing workspace-controlled hooks). Failing to gate
    /// here would let workspace hooks run without user approval in a
    /// subagent context.
    pub workspace_gated: bool,
    /// Manager for exec sessions (PTY and pipe).
    pub exec_sessions: ExecSessionManager,
    /// PTY session manager.
    pub pty_manager: PtyManager,
    /// Whether this controller manages a background runtime subprocess.
    pub managed_background_runtime: bool,
}

#[derive(Default)]
struct ProgressObserverRegistry {
    next_id: AtomicU64,
    observers: Mutex<BTreeMap<u64, RegisteredProgressObserver>>,
}

struct RegisteredProgressObserver {
    parent_session_id: String,
    observer: Arc<dyn SubagentProgressObserver>,
}

/// Keeps a progress observer registered with its subagent controller.
///
/// Dropping the registration detaches the observer. Adapters should retain it
/// for exactly the lifetime in which they want to receive status snapshots.
#[must_use = "dropping the registration detaches the progress observer"]
pub struct SubagentProgressObserverRegistration {
    registry: Weak<ProgressObserverRegistry>,
    id: u64,
}

impl Drop for SubagentProgressObserverRegistration {
    fn drop(&mut self) {
        if let Some(registry) = self.registry.upgrade() {
            let mut observers = registry.observers.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            observers.remove(&self.id);
        }
    }
}

/// Central controller that manages spawning, lifecycle, and state of all subagents.
#[derive(Clone)]
pub struct SubagentController {
    config: Arc<SubagentControllerConfig>,
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
    progress_observers: Arc<ProgressObserverRegistry>,
    /// Observer key inherited from the owning controller. Child-scoped
    /// controllers serve exactly one parent session, so their lifecycle
    /// events are published under that session's observer key even though the
    /// tasks' immediate `parent_thread_id` is the child's own session.
    progress_observer_scope: Option<String>,
}

impl SubagentController {
    /// Creates a new controller, discovering subagent specs and loading persisted background state.
    pub fn new(config: SubagentControllerConfig) -> impl Future<Output = Result<Self>> + Send {
        Self::new_with_background_owner(config, None)
    }

    /// Creates a controller scoped to one persisted background owner.
    ///
    /// Interactive ACP sessions use this constructor so a per-session
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
                load_background_state(&config.workspace_root)
                    .await?
                    .records
                    .into_iter()
                    .filter(|record| {
                        background_owner_session_id
                            .as_deref()
                            .is_none_or(|owner| record.owner_session_id.as_deref() == Some(owner))
                    })
                    .map(|mut record| {
                        record.canonicalize_id();
                        (record.id.clone(), BackgroundRecord::from_persisted(record))
                    })
                    .collect()
            };
            let parent_session_id = Arc::new(RwLock::new(config.parent_session_id.clone()));
            Ok(Self {
                config: Arc::new(config),
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

    /// Returns every child-scoped controller reachable from this controller's
    /// tracked children, recursively.
    ///
    /// Grandchildren spawned by a child run inside that child's own
    /// [`SubagentController`], so a caller that only reads `status_entries`
    /// sees direct children but not the rest of the owned tree. Management and
    /// lifecycle adapters walk this list to reach the whole subtree.
    ///
    /// Controllers are deduplicated by allocation so a re-parented or
    /// re-attached controller cannot produce a cycle, and the returned order
    /// is breadth-first from the nearest children outwards.
    pub async fn descendant_controllers(&self) -> Vec<Arc<SubagentController>> {
        let mut descendants = Vec::new();
        let mut visited = std::collections::HashSet::new();
        let mut frontier = {
            let state = self.state.read().await;
            state
                .children
                .values()
                .filter_map(|record| record.child_controller.clone())
                .collect::<Vec<_>>()
        };
        while !frontier.is_empty() {
            let mut next = Vec::new();
            for controller in frontier.drain(..) {
                if !visited.insert(Arc::as_ptr(&controller) as usize) {
                    continue;
                }
                let nested = {
                    let state = controller.state.read().await;
                    state
                        .children
                        .values()
                        .filter_map(|record| record.child_controller.clone())
                        .collect::<Vec<_>>()
                };
                next.extend(nested);
                descendants.push(controller);
            }
            frontier = next;
        }
        descendants
    }

    /// Shares lifecycle observation with the controller that owns this one.
    ///
    /// The nested-delegation loop creates a child-scoped controller per child
    /// run. That controller must publish its progress through the parent's
    /// observer registry, under the parent's observer key, so an adapter that
    /// registered for the root session still receives grandchild snapshots.
    /// Child-scoped controllers only ever serve their parent's session, so the
    /// inherited key cannot widen the observer's session boundary.
    pub(crate) async fn inherit_progress_observers(&mut self, parent: &SubagentController) {
        self.progress_observers = Arc::clone(&parent.progress_observers);
        self.progress_observer_scope = Some(match parent.progress_observer_scope.clone() {
            Some(scope) => scope,
            None => parent.parent_session_id.read().await.clone(),
        });
    }

    /// Attaches an already-created child-scoped controller to a tracked child.
    ///
    /// The nested-delegation loop attaches the controller as soon as the child
    /// run creates it so grandchildren stay reachable across resumes. Fails
    /// closed for an unknown child so a controller can never be grafted onto a
    /// record this controller does not track.
    pub async fn attach_child_controller(&self, child_id: &str, controller: Arc<SubagentController>) -> Result<()> {
        let mut state = self.state.write().await;
        Self::attach_child_controller_to_state(&mut state, child_id, controller)
    }

    pub(super) fn attach_child_controller_to_state(
        state: &mut ControllerState,
        child_id: &str,
        controller: Arc<SubagentController>,
    ) -> Result<()> {
        let record = state
            .children
            .get_mut(child_id)
            .ok_or_else(|| anyhow!("Unknown subagent id {child_id}"))?;
        record.child_controller = Some(controller);
        Ok(())
    }

    pub(super) async fn ensure_child_owned_by_invocation(&self, target: &str) -> Result<()> {
        let Some(context) = current_subagent_invocation_context() else {
            return Ok(());
        };
        let state = self.state.read().await;
        let record = state
            .children
            .get(target)
            .ok_or_else(|| anyhow!("Unknown subagent id {target}"))?;
        if record.parent_thread_id != context.parent_session_id {
            bail!("Unknown subagent id {target}");
        }
        Ok(())
    }

    /// Registers an observer for lifecycle snapshots belonging to one parent
    /// session. Separate registrations allow multiple adapters or sessions to
    /// observe the same controller without replacing one another.
    pub fn register_progress_observer(
        &self,
        parent_session_id: impl Into<String>,
        observer: Arc<dyn SubagentProgressObserver>,
    ) -> SubagentProgressObserverRegistration {
        let id = self.progress_observers.next_id.fetch_add(1, Ordering::Relaxed);
        let mut observers = self
            .progress_observers
            .observers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.insert(
            id,
            RegisteredProgressObserver {
                parent_session_id: parent_session_id.into(),
                observer,
            },
        );
        SubagentProgressObserverRegistration {
            registry: Arc::downgrade(&self.progress_observers),
            id,
        }
    }

    fn with_progress_observers(&self, parent_session_id: &str, notify: impl Fn(&dyn SubagentProgressObserver)) {
        // A child-scoped controller inherits its owner's observer key, so
        // grandchild events reach the adapter registered for the root session
        // instead of being dropped for an unknown intermediate session.
        let observer_key = self.progress_observer_scope.as_deref().unwrap_or(parent_session_id);
        let observers = {
            let observers = self
                .progress_observers
                .observers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            observers
                .values()
                .filter(|registration| registration.parent_session_id == observer_key)
                .map(|registration| Arc::clone(&registration.observer))
                .collect::<Vec<_>>()
        };
        for observer in observers {
            notify(observer.as_ref());
        }
    }

    pub(crate) async fn publish_subagent_progress(&self, task: SubagentStatusEntry) {
        self.with_progress_observers(&task.parent_thread_id, |observer| observer.subagent_status_changed(task.clone()));
    }

    pub(crate) async fn publish_background_progress(&self, task: BackgroundSubprocessEntry) {
        let parent_session_id = {
            let state = self.state.read().await;
            state
                .background_children
                .get(&task.id)
                .map(|record| record.parent_session_id.clone())
        };
        if let Some(parent_session_id) = parent_session_id.filter(|owner| !owner.is_empty()) {
            self.with_progress_observers(&parent_session_id, |observer| {
                observer.background_status_changed(task.clone())
            });
        }
    }

    pub(crate) async fn publish_child_status(&self, child_id: &str) {
        let entry = {
            let state = self.state.read().await;
            state.children.get(child_id).map(ChildRecord::build_status_entry)
        };
        if let Some(entry) = entry {
            self.publish_subagent_progress(entry).await;
        }
    }

    pub(crate) async fn publish_background_status(&self, record_id: &str) {
        let entry = {
            let state = self.state.read().await;
            state
                .background_children
                .get(record_id)
                .map(BackgroundRecord::build_status_entry)
        };
        if let Some(entry) = entry {
            self.publish_background_progress(entry).await;
        }
    }
}

// ─── Controller submodule split ────────────────────────────────────────────

mod controller_background_ops;
mod controller_child_loop;
mod controller_helpers;
mod controller_spawn_run;
mod controller_verify;

#[allow(
    unused_imports,
    reason = "Intentional compatibility, platform, or test-only suppression."
)]
pub(super) use controller_helpers::*;

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
