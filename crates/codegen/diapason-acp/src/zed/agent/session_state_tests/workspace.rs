//! Workspace behaviour for ACP session state.
//!
//! Shared fixtures live here; the tests themselves are grouped by concern in
//! the submodules below.
use super::*;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use crate::zed::agent::optional_setup::{OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes};
use crate::zed::agent::session_workspace::{WorkspaceRuntimeLifecycle, WorkspaceRuntimeLifecycleState};
use diapason_core::config::WorkspaceTrustLevel;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(Default)]
struct CountingWorkspaceTrustSynchronizer {
    calls: AtomicUsize,
}

#[async_trait::async_trait]
impl WorkspaceTrustSynchronizer for CountingWorkspaceTrustSynchronizer {
    async fn synchronize(
        &self,
        _workspace: &Path,
        desired_level: WorkspaceTrustLevel,
    ) -> anyhow::Result<WorkspaceTrustSyncOutcome> {
        let _previous_call_count = self.calls.fetch_add(1, Ordering::Relaxed);
        Ok(WorkspaceTrustSyncOutcome::AlreadyMatches(desired_level))
    }
}

/// Reference model for workspace-lease accounting and runtime lifecycle.
///
/// Each generated case builds an isolated shutdown-managed runtime, so there is
/// no baseline lease to carry around: the model starts from zero active
/// sessions and owns the whole count. A counted lease increments
/// `active_sessions` exactly once on acquisition and decrements it exactly once,
/// whether it leaves through `release` or `Drop`. Releasing an already-released
/// lease must not decrement a second time. Unmanaged leases never touch managed
/// accounting.
///
/// The lifecycle half pins the state machine that the count interacts with:
/// the final counted lease leaving an `Open` runtime moves it to
/// `ShutdownPending`, shutdown initiation moves it to `ShuttingDown` and then
/// `Closed`, and a `Closed` runtime never grants another lease.
///
/// The generated sequences are **sequential**. They assert bookkeeping
/// conservation over single-threaded operation order; they say nothing about
/// concurrent interleavings of acquisition, release and shutdown.
fn active_sessions(runtime: &SessionWorkspaceRuntime) -> usize {
    runtime
        .lifecycle
        .lock()
        .expect("workspace runtime lifecycle lock")
        .active_sessions
}

fn lifecycle_state(runtime: &SessionWorkspaceRuntime) -> WorkspaceRuntimeLifecycleState {
    runtime.lifecycle.lock().expect("workspace runtime lifecycle lock").state
}

/// Runtime used by the generated cases: shutdown-managed when `managed`,
/// otherwise an unmanaged runtime that must never touch the count.
///
/// A fresh `ToolRegistry` carries neither a subagent controller nor an MCP
/// client, so `spawn_shutdown` has no external side effects to await and the
/// shutdown task completes as soon as it is scheduled.
async fn isolated_runtime(managed: bool, workspace: &Path, agent: &ZedAgent) -> Arc<SessionWorkspaceRuntime> {
    let registry = diapason_core::tools::registry::ToolRegistry::new(workspace.to_path_buf()).await;
    Arc::new(SessionWorkspaceRuntime {
        workspace_root: workspace.to_path_buf(),
        workspace_hooks_gated: false,
        system_prompt: String::new(),
        primary_agents: agent.primary_agents.clone(),
        acp_tool_registry: Arc::clone(&agent.acp_tool_registry),
        permission_prompter: Arc::clone(&agent.permission_prompter),
        local_tool_registry: registry,
        file_ops_tool: None,
        optional_setup_outcomes: OptionalWorkspaceSetupOutcomes {
            subagent_controller: OptionalSetupOutcome::DisabledByConfiguration,
            mcp_client: OptionalSetupOutcome::DisabledByConfiguration,
        },
        shutdown_managed: managed,
        lifecycle: Mutex::new(WorkspaceRuntimeLifecycle {
            state: WorkspaceRuntimeLifecycleState::Open,
            active_sessions: 0,
        }),
        lifecycle_changed: tokio::sync::Notify::new(),
    })
}

/// Reference model for counted-lease accounting and runtime lifecycle.
///
/// Each generated case builds its own isolated shutdown-managed runtime, so no
/// baseline lease is carried through the cases: the model owns the whole count
/// from zero. A counted lease increments `active_sessions` exactly once on
/// acquisition and decrements it exactly once when it leaves, whether through
/// `release` or `Drop`. Because `release` consumes the lease, the same handle
/// can never decrement twice. Unmanaged leases never touch managed accounting.
///
/// The lifecycle half pins the state machine the count interacts with:
///
/// * `Drop` of the final counted lease only publishes `ShutdownPending`; it
///   neither initiates nor completes shutdown.
/// * `release` of the final counted lease drives shutdown to completion, so the
///   runtime is terminal when `release` returns.
/// * Acquiring from a `ShutdownPending` runtime initiates shutdown, waits for
///   it to complete and answers `None`, leaving the runtime terminal.
/// * A terminal runtime never grants another lease.
///
/// Every generated transition runs through the production `acquire`, `release`,
/// `Drop` and shutdown paths; the driver never assigns lifecycle state itself.
/// Each lease operation runs under [`LEASE_DEADLINE`] so that a suppressed
/// shutdown initiation fails the case instead of hanging it.
///
/// The generated sequences are **sequential**. They assert bookkeeping
/// conservation over single-threaded operation order; they say nothing about
/// concurrent interleavings of acquisition, release and shutdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeaseModelState {
    /// No counted lease is live and the runtime still accepts acquisitions.
    Open,
    /// The final counted lease left through `Drop`; shutdown is requested but
    /// not yet initiated.
    ShutdownPending,
    /// Terminal.
    Closed,
}

/// Upper bound for a single lease operation or acquisition.
///
/// Shutdown of an empty runtime completes immediately, so reaching this bound
/// means production stopped initiating or completing shutdown.
const LEASE_DEADLINE: Duration = Duration::from_secs(10);

#[derive(Debug)]
struct LeaseModel {
    active_sessions: usize,
    state: LeaseModelState,
}

impl Default for LeaseModel {
    fn default() -> Self {
        Self { active_sessions: 0, state: LeaseModelState::Open }
    }
}

impl LeaseModel {
    /// Predicts whether production must hand back a counted lease.
    fn acquire(&mut self) -> bool {
        match self.state {
            LeaseModelState::Open => {
                self.active_sessions += 1;
                true
            }
            // Acquisition from a pending runtime initiates shutdown and waits
            // for it to complete before answering, so the answer is "no" and
            // the runtime is terminal afterwards.
            LeaseModelState::ShutdownPending => {
                self.state = LeaseModelState::Closed;
                false
            }
            LeaseModelState::Closed => false,
        }
    }

    /// A counted lease leaves; `released` distinguishes `release` from `Drop`.
    fn leave(&mut self, released: bool) {
        assert!(self.active_sessions > 0, "reference model underflow");
        self.active_sessions -= 1;
        if self.active_sessions == 0 && self.state == LeaseModelState::Open {
            self.state = if released {
                LeaseModelState::Closed
            } else {
                LeaseModelState::ShutdownPending
            };
        }
    }

    /// Whether `actual` is an observation the model permits.
    fn admits(&self, actual: WorkspaceRuntimeLifecycleState) -> bool {
        match self.state {
            LeaseModelState::Open => actual == WorkspaceRuntimeLifecycleState::Open,
            LeaseModelState::ShutdownPending => actual == WorkspaceRuntimeLifecycleState::ShutdownPending,
            LeaseModelState::Closed => actual == WorkspaceRuntimeLifecycleState::Closed,
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum LeaseOp {
    Acquire,
    AcquireUnmanaged,
    Release,
    Drop,
}

/// Drive one lease operation to completion under [`LEASE_DEADLINE`].
///
/// The proptest closure is synchronous, so async production paths run through
/// the runtime handle. `block_in_place` requires a multi-threaded runtime,
/// which the generated cases declare.
fn bounded<F>(handle: &tokio::runtime::Handle, future: F) -> F::Output
where
    F: Future,
{
    tokio::task::block_in_place(|| {
        handle.block_on(async {
            tokio::time::timeout(LEASE_DEADLINE, future)
                .await
                .expect("a lease operation must not stall waiting for workspace shutdown")
        })
    })
}

#[path = "workspace_hooks.rs"]
mod hooks;

#[path = "workspace_lease.rs"]
mod lease;

#[path = "workspace_session.rs"]
mod session;



use diapason_config::{HookCommandConfig, HookGroupConfig, WorkspaceHookCommand, WorkspaceLifecycleHooks};

// weave: run 'weave explain crates/codegen/diapason-acp/src/zed/agent/session_state_tests/workspace.rs' for per-hunk detail, 'weave check' to verify your resolution
// weave: run 'weave explain crates/codegen/diapason-acp/src/zed/agent/session_state_tests/workspace.rs' for per-hunk detail, 'weave check' to verify your resolution
