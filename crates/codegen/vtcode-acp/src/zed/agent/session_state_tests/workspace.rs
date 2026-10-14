//! Workspace behavior for ACP session state.
use super::*;
use assert_fs::TempDir;
use std::sync::Arc;

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
async fn new_session_canonicalizes_and_isolates_requested_workspaces() {
    let launch_workspace = TempDir::new().unwrap();
    let first_workspace = TempDir::new().unwrap();
    let second_workspace = TempDir::new().unwrap();
    let agent = build_agent(launch_workspace.path()).await;

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
