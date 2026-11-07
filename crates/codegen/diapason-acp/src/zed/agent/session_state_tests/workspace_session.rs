//! Session creation, workspace isolation, and runtime sharing tests.
use super::*;
use assert_fs::TempDir;
use diapason_core::config::DiapasonConfig;

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
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let first_workspace = TempDir::new().expect("temporary first workspace");
    let second_workspace = TempDir::new().expect("temporary second workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let (first, second) = tokio::join!(
        agent.new_session(acp::NewSessionRequest::new(first_workspace.path())),
        agent.new_session(acp::NewSessionRequest::new(second_workspace.path()))
    );
    let first = first.expect("create the first ACP session");
    let second = second.expect("create the second ACP session");

    let first_session = agent
        .session_handle(&first.session_id)
        .expect("first session registry lookup")
        .expect("registered first session");
    let second_session = agent
        .session_handle(&second.session_id)
        .expect("second session registry lookup")
        .expect("registered second session");
    let first_runtime = first_session
        .workspace_runtime()
        .expect("first session workspace runtime lookup")
        .expect("first session workspace runtime");
    let second_runtime = second_session
        .workspace_runtime()
        .expect("second session workspace runtime lookup")
        .expect("second session workspace runtime");
    let first_metadata = first_session
        .data
        .lock()
        .expect("first session data lock")
        .thread
        .metadata()
        .expect("first thread metadata");
    let second_metadata = second_session
        .data
        .lock()
        .expect("second session data lock")
        .thread
        .metadata()
        .expect("second thread metadata");
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
                != diapason_commons::canonicalize(launch_workspace.path()).expect("canonicalize the launch workspace"),
        },
        WorkspaceSessionProjection {
            first_runtime_root: diapason_commons::canonicalize(first_workspace.path())
                .expect("canonicalize the first workspace"),
            second_runtime_root: diapason_commons::canonicalize(second_workspace.path())
                .expect("canonicalize the second workspace"),
            first_thread_workspace: diapason_commons::canonicalize(first_workspace.path())
                .expect("canonicalize the first workspace for its thread"),
            second_thread_workspace: diapason_commons::canonicalize(second_workspace.path())
                .expect("canonicalize the second workspace for its thread"),
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
    let workspace = TempDir::new().expect("temporary shared workspace");
    let agent = build_agent(workspace.path()).await;

    let first = agent
        .new_session(acp::NewSessionRequest::new(workspace.path()))
        .await
        .expect("create the first ACP session");
    let second = agent
        .new_session(acp::NewSessionRequest::new(workspace.path()))
        .await
        .expect("create the second ACP session");
    let first_runtime = agent
        .session_handle(&first.session_id)
        .expect("first session registry lookup")
        .expect("registered first session")
        .workspace_runtime()
        .expect("first session workspace runtime lookup")
        .expect("first session workspace runtime");
    let second_runtime = agent
        .session_handle(&second.session_id)
        .expect("second session registry lookup")
        .expect("registered second session")
        .workspace_runtime()
        .expect("second session workspace runtime lookup")
        .expect("second session workspace runtime");

    assert!(Arc::ptr_eq(&first_runtime, &second_runtime));
}

#[tokio::test]
async fn closing_only_the_last_session_shuts_down_its_workspace_runtime() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let session_workspace = TempDir::new().expect("temporary session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let first = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .expect("create the first ACP session");
    let second = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .expect("create the second ACP session");
    let runtime = agent
        .session_handle(&first.session_id)
        .expect("first session registry lookup")
        .expect("registered first session")
        .workspace_runtime()
        .expect("first session workspace runtime lookup")
        .expect("first session workspace runtime");

    let _close_response = agent
        .close_session(&first.session_id)
        .await
        .expect("close the first ACP session");
    assert!(!runtime.is_shutdown(), "one remaining session must retain its workspace runtime");

    let _close_response = agent
        .close_session(&second.session_id)
        .await
        .expect("close the final ACP session");
    assert!(runtime.is_shutdown(), "closing the last session should finish workspace runtime shutdown");
}

#[tokio::test]
async fn workspace_runtime_reservation_survives_close_before_session_registration() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let session_workspace = TempDir::new().expect("temporary session workspace");
    let agent = Arc::new(build_agent(launch_workspace.path()).await);

    let first = agent
        .new_session(acp::NewSessionRequest::new(session_workspace.path()))
        .await
        .expect("create the ACP session");
    let first_session = agent
        .session_handle(&first.session_id)
        .expect("session registry lookup")
        .expect("registered session");
    let runtime = first_session
        .workspace_runtime()
        .expect("session workspace runtime lookup")
        .expect("session workspace runtime");
    let selected_workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path())
        .await
        .expect("resolve the selected workspace");
    let reservation = agent
        .workspace_runtime_for(&selected_workspace)
        .await
        .expect("reserve the selected workspace runtime");
    assert!(Arc::ptr_eq(&runtime, reservation.runtime()));

    let _close_response = agent
        .close_session(&first.session_id)
        .await
        .expect("close the pending ACP session");
    assert!(
        !runtime.is_shutdown(),
        "a runtime lease acquired before registration must protect the pending session from close"
    );

    let second_session_id = agent
        .register_durable_session(reservation, None)
        .await
        .expect("register the reserved durable session");
    let _close_response = agent
        .close_session(&second_session_id)
        .await
        .expect("close the final registered session");
    assert!(runtime.is_shutdown(), "the final registered lease should shut down the runtime");
}

#[tokio::test]
async fn new_session_rejects_invalid_workspaces_without_registering_a_session() {
    let launch_workspace = TempDir::new().expect("temporary launch workspace");
    let agent = build_agent(launch_workspace.path()).await;
    let missing = launch_workspace.path().join("missing");

    let error = agent
        .new_session(acp::NewSessionRequest::new(missing))
        .await
        .expect_err("missing cwd must be rejected");

    assert_eq!(error.code, acp::ErrorCode::InvalidParams);
    assert!(agent.sessions.lock().expect("ACP sessions lock").is_empty());
}

#[tokio::test]
async fn checkpoint_session_skips_fresh_archive_when_history_persistence_is_disabled() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    for persistence in [HistoryPersistence::None, HistoryPersistence::Unknown] {
        let _history_settings = HistorySettingsGuard::set(persistence, None);
        let temp = TempDir::new().expect("temporary ACP workspace");
        let agent = build_agent(temp.path()).await;
        let session_id = agent
            .new_session(acp::NewSessionRequest::new(temp.path()))
            .await
            .expect("create ACP session")
            .session_id;
        let session = agent
            .session_handle(&session_id)
            .expect("session registry lookup")
            .expect("session handle");
        agent
            .push_message(&session, Message::user("do not persist".to_string()))
            .expect("push a user message");

        agent.checkpoint_session(&session).await.expect("checkpoint the session");

        assert!(session.data.lock().expect("session data lock").archive.is_none());
        assert!(
            find_session_by_identifier(&session_id.0)
                .await
                .expect("session archive lookup")
                .is_none()
        );
    }
}

#[tokio::test]
async fn resume_archive_is_disabled_when_history_persistence_is_disabled() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::None, None);
    let temp = TempDir::new().expect("temporary ACP workspace");
    let agent = build_agent(temp.path()).await;

    let result = agent
        .resume_session(acp::ResumeSessionRequest::new("archived-session", temp.path()))
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn new_session_does_not_restore_a_background_record_owned_by_another_session() {
    let temp = TempDir::new().expect("temporary ACP workspace");
    let state_dir = temp.path().join(".diapason/state");
    fs::create_dir_all(&state_dir).expect("create background state directory");
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
    .expect("write background state fixture");

    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let response = agent
        .new_session(acp::NewSessionRequest::new(temp.path()))
        .await
        .expect("create ACP session");
    let session = agent
        .session_handle(&response.session_id)
        .expect("session registry lookup")
        .expect("session handle");
    let controller = agent
        .session_subagent_controller(&session)
        .expect("subagent controller lookup")
        .expect("per-session subagent controller");

    assert!(controller.background_status_entries().await.is_empty());
}
