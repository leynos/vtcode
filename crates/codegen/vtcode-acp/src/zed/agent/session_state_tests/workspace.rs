//! Workspace behaviour for ACP session state.
use super::*;
use crate::workspace::{WorkspaceTrustSyncOutcome, WorkspaceTrustSynchronizer};
use assert_fs::TempDir;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
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

/// Reference model for workspace-lease accounting.
///
/// A counted lease increments `active_sessions` exactly once on acquisition and
/// decrements it exactly once, whether it leaves through `release` or `Drop`.
/// Releasing an already-released lease must not decrement a second time, and
/// unmanaged leases never touch the count. The generated sequences are
/// sequential; they assert bookkeeping conservation, not concurrency safety.
#[derive(Clone, Copy, Debug)]
enum LeaseOp {
    Acquire,
    Release,
    Drop,
}

fn active_sessions(runtime: &SessionWorkspaceRuntime) -> usize {
    runtime
        .lifecycle
        .lock()
        .expect("workspace runtime lifecycle lock")
        .active_sessions
}

#[tokio::test]
async fn workspace_lease_accounting_matches_the_reference_model() {
    use proptest::prelude::*;

    let launch_workspace = TempDir::new().expect("launch workspace");
    let session_workspace = TempDir::new().expect("session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    // One live session pins a shutdown-managed workspace runtime.
    let session = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .expect("create ACP session");
    let runtime = agent
        .session_handle(&session.session_id)
        .expect("look up session")
        .expect("registered session")
        .workspace_runtime()
        .expect("runtime state")
        .expect("workspace runtime");

    let strategy =
        prop::collection::vec(prop_oneof![Just(LeaseOp::Acquire), Just(LeaseOp::Release), Just(LeaseOp::Drop),], 0..8);

    let approved_workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path())
        .await
        .expect("approve session workspace");

    // The registered session already holds one counted lease; the model asserts
    // that additional leases move the count by exactly their live cardinality.
    let baseline = active_sessions(&runtime);
    assert!(baseline >= 1, "the session's own lease must be counted");

    proptest!(ProptestConfig { cases: 32, max_shrink_iters: 128, ..ProptestConfig::default() },
        |(ops in strategy)| {
            let mut live: Vec<SessionWorkspaceLease> = Vec::new();

            for op in ops {
                match op {
                    LeaseOp::Acquire => {
                        // Use the production acquisition path so the lease is
                        // counted exactly as the agent counts it.
                        let lease = futures::executor::block_on(
                            agent.workspace_runtime_for(&approved_workspace),
                        )
                        .expect("an open runtime must grant a lease");
                        live.push(lease);
                    }
                    LeaseOp::Release => {
                        if let Some(lease) = live.pop() {
                            futures::executor::block_on(lease.release());
                        }
                    }
                    LeaseOp::Drop => {
                        drop(live.pop());
                    }
                }

                prop_assert_eq!(active_sessions(&runtime), baseline + live.len());
            }

            // Dropping every extra lease must leave exactly the session's lease.
            drop(live);
            prop_assert_eq!(active_sessions(&runtime), baseline);
        }
    );

    drop(agent);
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

    let mut vt_config = vtcode_core::config::VTCodeConfig::default();
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
