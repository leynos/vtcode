//! Managed background subprocess spawn guards.
use super::*;

#[tokio::test]
async fn spawn_background_subprocess_rejects_non_background_agent() {
    let temp = TempDir::new().expect("tempdir");
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let err = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement a change.".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect_err("non-background agent should be rejected");

    assert!(err.to_string().contains("background: true"));
    assert!(err.to_string().contains("Use spawn_agent instead"));
}

#[tokio::test]
async fn spawn_background_subprocess_returns_active_record_when_settings_match() {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    // The record must name a session this process can observe. A record whose
    // session is absent is settled as a failure by refresh rather than reused,
    // so the reuse path is exercised with a live background session.
    let exec_sessions = controller.config.exec_sessions.clone();
    exec_sessions
        .create_pipe_session(
            "exec-session-background-demo".to_string().into(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            temp.path().to_path_buf(),
            hashbrown::HashMap::new(),
        )
        .await
        .expect("live background session");

    let spec = controller.resolve_requested_spec(Some("background-demo")).await.expect("spec");
    let record_id = background_record_id(spec.name.as_str());
    let created_at = Utc::now();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(
            record_id.clone(),
            BackgroundRecord {
                id: record_id.clone(),
                agent_name: spec.name.clone(),
                display_label: subagent_display_label(&spec),
                description: spec.description.clone(),
                source: spec.source.label(),
                colour: spec.colour.clone(),
                owner_session_id: None,
                session_id: "session-background-demo".to_string(),
                parent_session_id: "parent-session".to_string(),
                exec_session_id: "exec-session-background-demo".to_string(),
                desired_enabled: true,
                status: BackgroundSubprocessStatus::Running,
                created_at,
                updated_at: created_at,
                started_at: Some(created_at),
                ended_at: None,
                pid: Some(42),
                prompt: "Report readiness once.".to_string(),
                summary: Some("ready".to_string()),
                error: None,
                archive_path: None,
                transcript_path: None,
                max_turns: Some(4),
                model_override: None,
                reasoning_override: None,
                restart_attempts: 0,
            },
        );
    }

    let entry = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect("matching active record should be returned");

    assert_eq!(entry.id, record_id);
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);
    assert_eq!(
        entry.exec_session_id, "exec-session-background-demo",
        "the reused record must name the live launch, not a replacement this call started"
    );
    exec_sessions
        .close_session("exec-session-background-demo")
        .await
        .expect("close session");
}

#[tokio::test]
async fn spawn_background_subprocess_rejects_conflicting_active_record_settings() {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    // The record must name a session this process can observe. A record whose
    // session is absent is settled as a failure by refresh rather than reused,
    // so the reuse path is exercised with a live background session.
    let exec_sessions = controller.config.exec_sessions.clone();
    exec_sessions
        .create_pipe_session(
            "exec-session-background-demo".to_string().into(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
            temp.path().to_path_buf(),
            hashbrown::HashMap::new(),
        )
        .await
        .expect("live background session");

    let spec = controller.resolve_requested_spec(Some("background-demo")).await.expect("spec");
    let record_id = background_record_id(spec.name.as_str());
    let created_at = Utc::now();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(
            record_id,
            BackgroundRecord {
                id: background_record_id(spec.name.as_str()),
                agent_name: spec.name.clone(),
                display_label: subagent_display_label(&spec),
                description: spec.description.clone(),
                source: spec.source.label(),
                colour: spec.colour.clone(),
                owner_session_id: None,
                session_id: "session-background-demo".to_string(),
                parent_session_id: "parent-session".to_string(),
                exec_session_id: "exec-session-background-demo".to_string(),
                desired_enabled: true,
                status: BackgroundSubprocessStatus::Running,
                created_at,
                updated_at: created_at,
                started_at: Some(created_at),
                ended_at: None,
                pid: Some(42),
                prompt: "Report readiness once.".to_string(),
                summary: Some("ready".to_string()),
                error: None,
                archive_path: None,
                transcript_path: None,
                max_turns: Some(4),
                model_override: None,
                reasoning_override: None,
                restart_attempts: 0,
            },
        );
    }

    let err = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            message: Some("Run a different task.".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect_err("conflicting active record should be rejected");

    assert!(err.to_string().contains("different prompt"));
    assert!(err.to_string().contains("Stop or restart"));
    exec_sessions
        .close_session("exec-session-background-demo")
        .await
        .expect("close session");
}
