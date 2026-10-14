//! Tests for ACP session loading, resuming, workspace isolation, and lifecycle callbacks.

use super::*;

#[tokio::test]
async fn load_session_returns_existing_session_state() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();

    {
        let session = agent.session_handle(&session_id).unwrap().unwrap();
        let mut data = session.data.lock().unwrap();
        data.primary_agent = "build".to_string();
        data.reasoning_effort = ReasoningEffortLevel::High;
    }

    let args = LoadSessionRequest::new(session_id, temp.path());
    let response = agent.load_session(args).await.unwrap();

    assert!(response.modes.is_none());
    let config_options = response.config_options.unwrap();
    assert_eq!(config_options.len(), 4);
    assert!(config_options.iter().any(|option| {
        option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID)
            && matches!(
                &option.kind,
                SessionConfigKind::Select(select)
                    if select.current_value
                        == crate::acp::SessionConfigValueId::new("build")
            )
    }));
    assert!(config_options.iter().any(|option| {
        option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_THOUGHT_LEVEL_ID)
            && matches!(
                &option.kind,
                SessionConfigKind::Select(select)
                    if select.current_value
                        == crate::acp::SessionConfigValueId::new("high")
            )
    }));
    assert!(config_options.iter().any(|option| {
        option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PROVIDER_ID)
            && matches!(
                &option.kind,
                SessionConfigKind::Select(select)
                    if select.current_value
                        == crate::acp::SessionConfigValueId::new("openai")
            )
    }));
    assert!(config_options.iter().any(|option| {
        option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID)
            && matches!(
                &option.kind,
                SessionConfigKind::Select(select)
                    if select.current_value
                        == crate::acp::SessionConfigValueId::new("gpt-5.6-sol")
            )
    }));
}

#[tokio::test]
async fn resume_session_returns_existing_session_state() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();
    session
        .data
        .lock()
        .unwrap()
        .thread
        .append_message(vtcode_core::llm::provider::Message::user("continue".to_string()));

    let response = agent
        .resume_session(ResumeSessionRequest::new(session_id.clone(), temp.path()))
        .await
        .unwrap();

    assert!(response.modes.is_none());
    assert!(response.config_options.is_some());
    assert!(
        session
            .data
            .lock()
            .unwrap()
            .thread
            .messages()
            .iter()
            .any(|message| message.content.as_text() == "continue")
    );
}

#[tokio::test]
async fn resume_session_rejects_a_different_workspace() {
    let temp = TempDir::new().unwrap();
    let other = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();

    let result = agent.resume_session(ResumeSessionRequest::new(session_id, other.path())).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn lifecycle_hooks_are_attached_to_new_acp_sessions_and_end_on_connection_shutdown() {
    let temp = TempDir::new().unwrap();
    let marker = temp.path().join("acp-hook-events.log");
    let mut vt_cfg = VTCodeConfig::default();
    let command = format!("printf '%s\\n' \"$VT_HOOK_EVENT:$VT_SESSION_ID\" >> '{}'", marker.display());
    vt_cfg.hooks.lifecycle.session_start = vec![HookGroupConfig {
        matcher: Some("new_session".to_string()),
        hooks: vec![HookCommandConfig {
            command: command.clone(),
            ..HookCommandConfig::default()
        }],
    }];
    vt_cfg.hooks.lifecycle.session_end = vec![HookGroupConfig {
        matcher: Some("exit".to_string()),
        hooks: vec![HookCommandConfig { command, ..HookCommandConfig::default() }],
    }];

    let agent = build_agent_with_vt_config(temp.path(), Some(&vt_cfg)).await;
    let response = agent.new_session(NewSessionRequest::new(temp.path())).await.unwrap();
    let session_id = response.session_id;
    let session = agent.session_handle(&session_id).unwrap().unwrap();
    assert!(session.lifecycle_hooks().unwrap().is_some(), "new ACP session should have its own lifecycle engine");
    assert_eq!(fs::read_to_string(&marker).await.unwrap().lines().count(), 1);

    agent
        .run_session_end_hooks(vtcode_core::hooks::SessionEndReason::Exit)
        .await
        .unwrap();
    agent
        .run_session_end_hooks(vtcode_core::hooks::SessionEndReason::Exit)
        .await
        .unwrap();
    let events = fs::read_to_string(&marker).await.unwrap();
    assert_eq!(events.lines().count(), 2);
    assert!(events.lines().all(|line| line.contains(&session_id.to_string())));
}
