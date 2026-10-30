//! Workspace behaviour for ACP session state.
use super::*;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use crate::zed::agent::optional_setup::{OptionalSetupOutcome, OptionalWorkspaceSetupOutcomes};
use crate::zed::agent::session_workspace::{WorkspaceRuntimeLifecycle, WorkspaceRuntimeLifecycleState};
use assert_fs::TempDir;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use vtcode_core::config::WorkspaceTrustLevel;

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
    let registry = vtcode_core::tools::registry::ToolRegistry::new(workspace.to_path_buf()).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn workspace_lease_accounting_matches_the_reference_model() {
    use proptest::prelude::*;

    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);
    let approved_workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path())
        .await
        .expect("approve session workspace");

    let strategy = prop::collection::vec(
        prop_oneof![
            Just(LeaseOp::Acquire),
            Just(LeaseOp::AcquireUnmanaged),
            Just(LeaseOp::Release),
            Just(LeaseOp::Drop),
        ],
        0..12,
    );

    let handle = tokio::runtime::Handle::current();

    proptest!(ProptestConfig { cases: 32, max_shrink_iters: 256, ..ProptestConfig::default() },
        |(ops in strategy)| {
            let mut model = LeaseModel::default();
            let mut live: Vec<SessionWorkspaceLease> = Vec::new();

            let managed = tokio::task::block_in_place(|| {
                handle.block_on(isolated_runtime(true, session_workspace.path(), &agent))
            });
            let unmanaged = tokio::task::block_in_place(|| {
                handle.block_on(isolated_runtime(false, session_workspace.path(), &agent))
            });

            for op in ops {
                match op {
                    LeaseOp::Acquire => {
                        let model_grants = model.acquire();
                        let granted = bounded(&handle, SessionWorkspaceLease::acquire(Arc::clone(&managed)))
                            .expect("acquiring a managed lease must not fail");
                        prop_assert_eq!(
                            granted.is_some(),
                            model_grants,
                            "acquisition must match the model, which expects {:?}",
                            model.state
                        );
                        if let Some(lease) = granted {
                            live.push(lease);
                        }
                    }
                    LeaseOp::AcquireUnmanaged => {
                        let lease = bounded(&handle, SessionWorkspaceLease::acquire(Arc::clone(&unmanaged)))
                            .expect("acquiring an unmanaged lease must not fail")
                        .expect("unmanaged runtimes always grant a lease");
                        prop_assert!(
                            !lease.is_counted(),
                            "an unmanaged runtime must hand back an uncounted lease"
                        );
                        drop(lease);
                        prop_assert_eq!(
                            active_sessions(&unmanaged),
                            0,
                            "unmanaged leases must never touch unmanaged accounting"
                        );
                    }
                    LeaseOp::Release => {
                        if let Some(lease) = live.pop() {
                            bounded(&handle, lease.release());
                            model.leave(true);
                        }
                    }
                    LeaseOp::Drop => {
                        if let Some(lease) = live.pop() {
                            drop(lease);
                            model.leave(false);
                        }
                    }
                }

                prop_assert_eq!(
                    active_sessions(&managed),
                    model.active_sessions,
                    "live counted leases must match the model after {:?}",
                    op
                );
                let state = lifecycle_state(&managed);
                prop_assert!(
                    model.admits(state),
                    "runtime state {state:?} is not admitted by model state {:?} after {:?}",
                    model.state,
                    op
                );
            }

            // Drain, asserting each departure decrements exactly once. `release`
            // consumes the lease, so the following `Drop` cannot decrement the
            // same handle a second time.
            while let Some(lease) = live.pop() {
                let before = active_sessions(&managed);
                bounded(&handle, lease.release());
                prop_assert_eq!(
                    active_sessions(&managed),
                    before - 1,
                    "explicit release must decrement exactly one accounted lease"
                );
                model.leave(true);
            }
            prop_assert_eq!(active_sessions(&managed), 0, "the drained runtime must reach zero");
            let drained_state = lifecycle_state(&managed);
            prop_assert!(
                model.admits(drained_state),
                "runtime state {drained_state:?} is not admitted by model state {:?} after draining",
                model.state
            );
        }
    );

    // Deterministic: a terminal runtime refuses new leases, and that refusal is
    // distinct from the agent manufacturing a replacement runtime for the same
    // workspace.
    let lease = agent
        .workspace_runtime_for(&approved_workspace)
        .await
        .expect("the agent must create a workspace runtime");
    let runtime = Arc::clone(lease.runtime());
    assert!(lease.is_counted(), "a cached workspace runtime lease is counted");
    assert_eq!(active_sessions(&runtime), 1);

    runtime.mark_shutdown_complete();
    let refused = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring from a terminal runtime must not error");
    assert!(refused.is_none(), "a terminal runtime must not grant another counted lease");
    assert_eq!(active_sessions(&runtime), 1, "a refused acquisition must leave the count untouched");

    lease.release().await;
    assert_eq!(active_sessions(&runtime), 0, "explicit release must decrement exactly one accounted lease");
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::Closed,
        "an explicit release after terminal shutdown must not reopen the lifecycle"
    );

    let replacement = agent
        .workspace_runtime_for(&approved_workspace)
        .await
        .expect("the agent must replace a terminal runtime");
    assert!(
        !Arc::ptr_eq(replacement.runtime(), &runtime),
        "workspace_runtime_for must not hand back a terminal runtime"
    );
    assert_eq!(active_sessions(replacement.runtime()), 1, "the replacement runtime must start a fresh count");
    replacement.release().await;
    drop(runtime);
}

/// Explicit release consumes the lease, so its later destruction cannot
/// decrement the count a second time or revive a finished lifecycle.
#[tokio::test]
async fn explicit_release_then_destruction_does_not_decrement_twice() {
    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let keeper = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    let released = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a second managed lease must not fail")
        .expect("an open runtime must grant a lease");
    assert_eq!(active_sessions(&runtime), 2);

    // Releasing one of two leaves the count and the lifecycle untouched.
    released.release().await;
    assert_eq!(active_sessions(&runtime), 1);
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Open);

    // The final lease drives shutdown to completion before `release` returns.
    keeper.release().await;
    assert_eq!(active_sessions(&runtime), 0);
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Closed);

    // Dropping the runtime handle now must not re-run either decrement.
    drop(runtime);

    // A `Drop`-only departure publishes `ShutdownPending` and stops there.
    let dropped_runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let lease = SessionWorkspaceLease::acquire(Arc::clone(&dropped_runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    drop(lease);
    assert_eq!(active_sessions(&dropped_runtime), 0);
    assert_eq!(
        lifecycle_state(&dropped_runtime),
        WorkspaceRuntimeLifecycleState::ShutdownPending,
        "a dropped final lease must request shutdown without driving it"
    );
}

/// Acquiring from a runtime whose final lease went through `Drop` must initiate
/// shutdown, wait for it to finish and refuse the caller; the now-terminal
/// runtime must keep refusing afterwards.
#[tokio::test]
async fn acquisition_from_a_pending_shutdown_closes_the_runtime_and_refuses() {
    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let runtime = isolated_runtime(true, session_workspace.path(), &agent).await;
    let lease = SessionWorkspaceLease::acquire(Arc::clone(&runtime))
        .await
        .expect("acquiring a managed lease must not fail")
        .expect("an open runtime must grant a lease");
    drop(lease);
    assert_eq!(active_sessions(&runtime), 0);
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::ShutdownPending,
        "the drop must leave shutdown requested but not initiated"
    );

    let refused = tokio::time::timeout(LEASE_DEADLINE, SessionWorkspaceLease::acquire(Arc::clone(&runtime)))
        .await
        .expect("acquisition must not stall waiting for the shutdown it initiated")
        .expect("acquiring from a pending runtime must not error");
    assert!(refused.is_none(), "a pending runtime must refuse the old runtime's lease");
    assert_eq!(
        lifecycle_state(&runtime),
        WorkspaceRuntimeLifecycleState::Closed,
        "acquisition must drive the pending shutdown to completion"
    );
    assert_eq!(active_sessions(&runtime), 0, "a refused acquisition must not be counted");

    let refused_again = tokio::time::timeout(LEASE_DEADLINE, SessionWorkspaceLease::acquire(Arc::clone(&runtime)))
        .await
        .expect("acquiring from a terminal runtime must answer without waiting")
        .expect("acquiring from a terminal runtime must not error");
    assert!(refused_again.is_none(), "a terminal runtime must keep refusing leases");
    assert_eq!(lifecycle_state(&runtime), WorkspaceRuntimeLifecycleState::Closed);
}

#[derive(Debug, PartialEq)]
struct WorkspaceSessionProjection {
    first_runtime_root: std::path::PathBuf,
    second_runtime_root: std::path::PathBuf,
    first_thread_workspace: std::path::PathBuf,
    second_thread_workspace: std::path::PathBuf,
    runtimes_are_distinct: bool,
    roots_are_distinct: bool,
    launch_workspace_is_unselected: bool,
}

#[tokio::test]
async fn new_session_discovery_failure_does_not_expose_workspace_path() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let requested_workspace = TempDir::new().expect("temporary selected workspace");
    let agent = build_agent(launch_workspace.path()).await;
    let agents_directory = requested_workspace.path().join(".codex/agents");
    fs::create_dir_all(&agents_directory).expect("create workspace agents directory");
    fs::write(agents_directory.join("private-agent-path-sentinel.toml"), b"\xff")
        .expect("write unreadable workspace agent");

    let error = agent
        .new_session(acp::NewSessionRequest::new(requested_workspace.path()))
        .await
        .expect_err("invalid UTF-8 workspace agent must fail discovery");
    let surfaced = format!("{error:?}");
    assert!(surfaced.contains("workspace_runtime_setup_failed"));
    assert!(!surfaced.contains(&requested_workspace.path().display().to_string()));
    assert!(!surfaced.contains("private-agent-path-sentinel"));
}

#[tokio::test]
async fn new_session_canonicalizes_and_isolates_requested_workspaces() {
    let launch_workspace = TempDir::new().unwrap();
    let first_workspace = TempDir::new().unwrap();
    let second_workspace = TempDir::new().unwrap();
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let (first, second) = tokio::join!(
        agent.new_session(acp::NewSessionRequest::new(first_workspace.path())),
        agent.new_session(acp::NewSessionRequest::new(second_workspace.path()))
    );
    let first = first.unwrap();
    let second = second.unwrap();

    let first_session = agent.session_handle(&first.session_id).unwrap().unwrap();
    let second_session = agent.session_handle(&second.session_id).unwrap().unwrap();
    let first_runtime = first_session.workspace_runtime().unwrap().unwrap();
    let second_runtime = second_session.workspace_runtime().unwrap().unwrap();
    let first_metadata = first_session.data.lock().unwrap().thread.metadata().unwrap();
    let second_metadata = second_session.data.lock().unwrap().thread.metadata().unwrap();
    let first_root = first_runtime.workspace_root.clone();
    let second_root = second_runtime.workspace_root.clone();
    assert_eq!(
        WorkspaceSessionProjection {
            first_runtime_root: first_root.clone(),
            second_runtime_root: second_root.clone(),
            first_thread_workspace: Path::new(&first_metadata.workspace_path).to_path_buf(),
            second_thread_workspace: Path::new(&second_metadata.workspace_path).to_path_buf(),
            runtimes_are_distinct: !Arc::ptr_eq(&first_runtime, &second_runtime),
            roots_are_distinct: first_root != second_root,
            launch_workspace_is_unselected: first_root
                != vtcode_commons::canonicalize(launch_workspace.path()).unwrap(),
        },
        WorkspaceSessionProjection {
            first_runtime_root: vtcode_commons::canonicalize(first_workspace.path()).unwrap(),
            second_runtime_root: vtcode_commons::canonicalize(second_workspace.path()).unwrap(),
            first_thread_workspace: vtcode_commons::canonicalize(first_workspace.path()).unwrap(),
            second_thread_workspace: vtcode_commons::canonicalize(second_workspace.path()).unwrap(),
            runtimes_are_distinct: true,
            roots_are_distinct: true,
            launch_workspace_is_unselected: true,
        }
    );
}

#[tokio::test]
async fn disallowed_workspace_is_rejected_before_trust_is_synchronized() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let requested_workspace = TempDir::new().expect("temporary unconfigured workspace");
    let mut agent = build_agent(launch_workspace.path()).await;
    agent.workspace_policy = AcpWorkspacePolicy::new(launch_workspace.path(), &[])
        .await
        .expect("policy limited to the launch workspace");
    let synchronizer = Arc::new(CountingWorkspaceTrustSynchronizer::default());
    let synchronizer_handle: Arc<dyn WorkspaceTrustSynchronizer + Send + Sync> = synchronizer.clone();
    agent.workspace_trust_synchronizer = synchronizer_handle;

    let error = agent
        .new_session(acp::NewSessionRequest::new(requested_workspace.path()))
        .await
        .expect_err("unconfigured workspace should be rejected");

    assert_eq!(synchronizer.calls.load(Ordering::Relaxed), 0);
    assert!(agent.sessions.lock().expect("ACP sessions lock").is_empty());
    drop(error);
}

#[tokio::test]
async fn sessions_in_the_same_workspace_share_the_workspace_runtime() {
    let workspace = TempDir::new().unwrap();
    let agent = build_agent(workspace.path()).await;

    let first = agent.new_session(acp::NewSessionRequest::new(workspace.path())).await.unwrap();
    let second = agent.new_session(acp::NewSessionRequest::new(workspace.path())).await.unwrap();
    let first_runtime = agent
        .session_handle(&first.session_id)
        .unwrap()
        .unwrap()
        .workspace_runtime()
        .unwrap()
        .unwrap();
    let second_runtime = agent
        .session_handle(&second.session_id)
        .unwrap()
        .unwrap()
        .workspace_runtime()
        .unwrap()
        .unwrap();

    assert!(Arc::ptr_eq(&first_runtime, &second_runtime));
}

#[tokio::test]
async fn closing_only_the_last_session_shuts_down_its_workspace_runtime() {
    let launch_workspace = TempDir::new().unwrap();
    let session_workspace = TempDir::new().unwrap();
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let first = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .unwrap();
    let second = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .unwrap();
    let runtime = agent
        .session_handle(&first.session_id)
        .unwrap()
        .unwrap()
        .workspace_runtime()
        .unwrap()
        .unwrap();

    let _close_response = agent.close_session(&first.session_id).await.unwrap();
    assert!(!runtime.is_shutdown(), "one remaining session must retain its workspace runtime");

    let _close_response = agent.close_session(&second.session_id).await.unwrap();
    assert!(runtime.is_shutdown(), "closing the last session should finish workspace runtime shutdown");
}

#[tokio::test]
async fn workspace_runtime_reservation_survives_close_before_session_registration() {
    let launch_workspace = TempDir::new().unwrap();
    let session_workspace = TempDir::new().unwrap();
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let first = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .unwrap();
    let first_session = agent.session_handle(&first.session_id).unwrap().unwrap();
    let runtime = first_session.workspace_runtime().unwrap().unwrap();
    let selected_workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path()).await.unwrap();
    let reservation = agent.workspace_runtime_for(&selected_workspace).await.unwrap();
    assert!(Arc::ptr_eq(&runtime, reservation.runtime()));

    let _close_response = agent.close_session(&first.session_id).await.unwrap();
    assert!(
        !runtime.is_shutdown(),
        "a runtime lease acquired before registration must protect the pending session from close"
    );

    let second_session_id = agent.register_durable_session(reservation, None).await.unwrap();
    let _close_response = agent.close_session(&second_session_id).await.unwrap();
    assert!(runtime.is_shutdown(), "the final registered lease should shut down the runtime");
}

#[tokio::test]
async fn new_session_restores_matching_workspace_hook_approval_before_session_start() {
    let workspace = TempDir::new().unwrap();
    let dot_config = TempDir::new().unwrap();
    let marker = workspace.path().join("approved-session-start");
    let command = format!("touch {}", marker.display());
    let env_guard = vtcode_commons::env_lock::lock();
    let previous = std::env::var_os("VTCODE_CONFIG");
    env_guard.set_var("VTCODE_CONFIG", dot_config.path().join("config").to_string_lossy().into_owned());
    vtcode_core::utils::dot_config::initialize_dot_folder().await.unwrap();

    let mut vt_config = VTCodeConfig::default();
    vt_config
        .hooks
        .lifecycle
        .session_start
        .push(vtcode_core::config::HookGroupConfig {
            matcher: None,
            hooks: vec![vtcode_core::config::HookCommandConfig {
                kind: Default::default(),
                command: command.clone(),
                timeout_seconds: None,
            }],
        });
    vt_config.workspace_lifecycle_hooks = Some(vtcode_config::hooks::WorkspaceLifecycleHooks {
        commands: vec![vtcode_config::hooks::WorkspaceHookCommand {
            event: "session_start".to_string(),
            matcher: None,
            command: command.clone(),
            timeout_seconds: None,
        }],
    });
    let preview = LifecycleHookEngine::new_with_session_gated(
        workspace.path().to_path_buf(),
        &vt_config.hooks,
        SessionStartTrigger::NewSession,
        "approval-preview",
        true,
    )
    .unwrap()
    .unwrap();
    vtcode_core::utils::dot_config::update_lifecycle_hook_approval(
        workspace.path(),
        preview.command_digest().to_string(),
    )
    .await
    .unwrap();

    let agent = build_agent_with_vt_config(workspace.path(), "duck", Some(&vt_config)).await;
    let result = agent.new_session(acp::NewSessionRequest::new(workspace.path())).await;
    let marker_was_created = marker.exists();
    env_guard.restore_var("VTCODE_CONFIG", previous);

    assert!(result.is_ok(), "session should start after restoring its matching hook approval");
    assert!(marker_was_created, "matching persisted approval must be restored before SessionStart runs");
}

#[tokio::test]
async fn new_session_rejects_invalid_workspaces_without_registering_a_session() {
    let launch_workspace = TempDir::new().unwrap();
    let agent = build_agent(launch_workspace.path()).await;
    let missing = launch_workspace.path().join("missing");

    let error = agent
        .new_session(acp::NewSessionRequest::new(missing))
        .await
        .expect_err("missing cwd must be rejected");

    assert_eq!(error.code, acp::ErrorCode::InvalidParams);
    assert!(agent.sessions.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checkpoint_session_skips_fresh_archive_when_history_persistence_is_disabled() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    for persistence in [HistoryPersistence::None, HistoryPersistence::Unknown] {
        let _history_settings = HistorySettingsGuard::set(persistence, None);
        let temp = TempDir::new().unwrap();
        let agent = build_agent(temp.path()).await;
        let session_id = agent
            .new_session(acp::NewSessionRequest::new(temp.path()))
            .await
            .unwrap()
            .session_id;
        let session = agent.session_handle(&session_id).unwrap().unwrap();
        agent
            .push_message(&session, Message::user("do not persist".to_string()))
            .unwrap();

        agent.checkpoint_session(&session).await.unwrap();

        assert!(session.data.lock().unwrap().archive.is_none());
        assert!(find_session_by_identifier(&session_id.0).await.unwrap().is_none());
    }
}

#[tokio::test]
async fn resume_archive_is_disabled_when_history_persistence_is_disabled() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::None, None);
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    let result = agent
        .resume_session(acp::ResumeSessionRequest::new("archived-session", temp.path()))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn new_session_does_not_restore_a_background_record_owned_by_another_session() {
    let temp = TempDir::new().unwrap();
    let state_dir = temp.path().join(".vtcode/state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("background_subagents.json"),
        serde_json::json!({
            "records": [{
                "id": "foreign-worker",
                "agent_name": "worker",
                "display_label": "worker",
                "description": "worker",
                "source": "builtin",
                "color": null,
                "owner_session_id": "prior-session",
                "session_id": "child-session",
                "exec_session_id": "exec-session",
                "desired_enabled": true,
                "status": "stopped",
                "created_at": "2026-08-28T00:00:00Z",
                "updated_at": "2026-08-28T00:00:00Z",
                "started_at": null,
                "ended_at": null,
                "pid": null,
                "prompt": "work",
                "summary": null,
                "error": null,
                "archive_path": null,
                "transcript_path": null,
                "max_turns": null,
                "model_override": null,
                "reasoning_override": null,
                "restart_attempts": 0
            }]
        })
        .to_string(),
    )
    .unwrap();

    let mut vt_config = VTCodeConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let controller = agent
        .session_subagent_controller(&session)
        .unwrap()
        .expect("per-session subagent controller");

    assert!(controller.background_status_entries().await.is_empty());
}
