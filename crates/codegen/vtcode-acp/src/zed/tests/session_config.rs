//! Tests for ACP session configuration options and model-facing settings.

use super::*;
use std::path::Path;

async fn build_agent_with_session(workspace: &Path) -> (ZedAgent, crate::acp::SessionId) {
    let agent = build_agent(workspace).await;
    let session_id = agent.register_session().unwrap();
    (agent, session_id)
}

fn config_option_has_current_value(
    config_options: &[crate::acp::SessionConfigOption],
    config_id: &str,
    expected_value: &str,
) -> bool {
    config_options.iter().any(|option| {
        option.id == crate::acp::SessionConfigId::new(config_id)
            && matches!(
                &option.kind,
                SessionConfigKind::Select(select)
                    if select.current_value == crate::acp::SessionConfigValueId::new(expected_value)
            )
    })
}

fn assert_session_config_option_is_current(
    current_session_value: &str,
    config_options: &[crate::acp::SessionConfigOption],
    config_id: crate::acp::SessionConfigId,
    expected_value: crate::acp::SessionConfigValueId,
) {
    assert_eq!(current_session_value, expected_value.0.as_ref(), "session config state");
    assert!(
        config_option_has_current_value(config_options, config_id.0.as_ref(), expected_value.0.as_ref()),
        "ACP config option {} should advertise {} as its current value",
        config_id.0,
        expected_value.0,
    );
}

#[tokio::test]
async fn new_session_returns_config_options() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    let response = agent.new_session(NewSessionRequest::new(temp.path())).await.unwrap();

    assert!(response.modes.is_none());
    let config_options = response.config_options.unwrap();
    assert_eq!(config_options.len(), 4);
    assert!(config_option_has_current_value(&config_options, SESSION_CONFIG_PRIMARY_AGENT_ID, "duck"));
    assert!(config_option_has_current_value(&config_options, SESSION_CONFIG_THOUGHT_LEVEL_ID, "low"));
    assert!(config_option_has_current_value(&config_options, SESSION_CONFIG_PROVIDER_ID, "openai"));
    assert!(config_option_has_current_value(&config_options, SESSION_CONFIG_MODEL_ID, "gpt-5.6-sol"));
}

mod primary_agent_options {
    use super::*;

    fn primary_agent_select_options(
        config_options: &[crate::acp::SessionConfigOption],
    ) -> (Vec<(String, String)>, Option<String>) {
        let Some(select) = config_options.iter().find_map(|option| {
            if option.id == crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID) {
                match &option.kind {
                    SessionConfigKind::Select(select) => Some(select),
                    _ => None,
                }
            } else {
                None
            }
        }) else {
            return (Vec::new(), None);
        };

        let options = match &select.options {
            SessionConfigSelectOptions::Ungrouped(options) => options
                .iter()
                .map(|option| (option.value.0.as_ref().to_string(), option.name.clone()))
                .collect(),
            _ => Vec::new(),
        };
        (options, Some(select.current_value.0.as_ref().to_string()))
    }

    fn primary_agent_select_values(config_options: &[crate::acp::SessionConfigOption]) -> Vec<String> {
        primary_agent_select_options(config_options)
            .0
            .into_iter()
            .map(|(value, _)| value)
            .collect()
    }

    fn primary_agent_select_labels(config_options: &[crate::acp::SessionConfigOption]) -> Vec<String> {
        primary_agent_select_options(config_options)
            .0
            .into_iter()
            .map(|(_, label)| label)
            .collect()
    }

    fn primary_agent_current_value(config_options: &[crate::acp::SessionConfigOption]) -> Option<String> {
        primary_agent_select_options(config_options).1
    }

    #[tokio::test]
    async fn set_session_config_option_updates_primary_agent() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "build",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap().unwrap();
        assert_session_config_option_is_current(
            &session.data.lock().unwrap().primary_agent,
            &response.config_options,
            crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID),
            crate::acp::SessionConfigValueId::new("build"),
        );
    }

    #[tokio::test]
    async fn set_session_config_option_accepts_only_known_primary_agent_ids() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;
        let session = agent.session_handle(&session_id).unwrap().unwrap();

        for primary_agent in ["duck", "plan", "build", "auto"] {
            let response = agent
                .set_session_config_option(SetSessionConfigOptionRequest::new(
                    session_id.clone(),
                    SESSION_CONFIG_PRIMARY_AGENT_ID,
                    primary_agent,
                ))
                .await
                .unwrap();

            assert_session_config_option_is_current(
                &session.data.lock().unwrap().primary_agent,
                &response.config_options,
                crate::acp::SessionConfigId::new(SESSION_CONFIG_PRIMARY_AGENT_ID),
                crate::acp::SessionConfigValueId::new(primary_agent),
            );
        }
    }

    #[tokio::test]
    async fn set_session_config_option_rejects_unknown_primary_agent() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;
        let session = agent.session_handle(&session_id).unwrap().unwrap();
        session.data.lock().unwrap().primary_agent = "build".to_string();

        let result = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "research",
            ))
            .await;

        let error = result.expect_err("unknown primary agent should be rejected");
        let error = format!("{error:?}");
        assert!(error.contains("unknown_primary_agent"));
        assert!(error.contains("research"));
        assert_eq!(session.data.lock().unwrap().primary_agent, "build");
    }

    #[tokio::test]
    async fn session_config_options_include_custom_primary_agent() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).await.unwrap();
        fs::write(
            temp.path().join(".vtcode/agents/research.md"),
            r#"---
name: research
description: Research primary
mode: primary
permissions:
  default: deny
---
Research primary prompt."#,
        )
        .await
        .unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;
        let session = agent.session_handle(&session_id).unwrap().unwrap();
        session.data.lock().unwrap().primary_agent = "research".to_string();

        let response = agent
            .load_session(LoadSessionRequest::new(session_id, temp.path()))
            .await
            .unwrap();
        let config_options = response.config_options.unwrap();

        assert_eq!(
            primary_agent_select_values(&config_options),
            ["duck", "plan", "build", "auto", "research"].map(str::to_string).to_vec()
        );
        assert!(primary_agent_select_labels(&config_options).contains(&"Research primary".into()));
        assert_eq!(primary_agent_current_value(&config_options), Some("research".to_string()));
    }

    #[tokio::test]
    async fn session_config_options_use_overridden_builtin_primary_agent_metadata() {
        let temp = TempDir::new().unwrap();
        fs::create_dir_all(temp.path().join(".vtcode/agents")).await.unwrap();
        fs::write(
            temp.path().join(".vtcode/agents/build.md"),
            r#"---
name: build
description: Project Build
mode: primary
permissions:
  default: deny
aliases:
  - project-builder
---
Project build prompt."#,
        )
        .await
        .unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;
        let session = agent.session_handle(&session_id).unwrap().unwrap();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id,
                SESSION_CONFIG_PRIMARY_AGENT_ID,
                "project-builder",
            ))
            .await
            .unwrap();

        assert_eq!(session.data.lock().unwrap().primary_agent, "build");
        assert!(primary_agent_select_labels(&response.config_options).contains(&"Project Build".into()));
    }
}

mod model_options {
    use super::*;

    #[tokio::test]
    async fn set_session_config_option_updates_reasoning_effort() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_THOUGHT_LEVEL_ID,
                "xhigh",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap().unwrap();
        assert_session_config_option_is_current(
            session.data.lock().unwrap().reasoning_effort.as_str(),
            &response.config_options,
            crate::acp::SessionConfigId::new(SESSION_CONFIG_THOUGHT_LEVEL_ID),
            crate::acp::SessionConfigValueId::new("xhigh"),
        );
    }

    #[tokio::test]
    async fn set_session_config_option_updates_provider_and_auto_switches_model() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;
        let anthropic_default = ModelId::default_single_for_provider(Provider::Anthropic).as_str().into_owned();

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_PROVIDER_ID,
                "anthropic",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap().unwrap();
        assert_eq!(session.data.lock().unwrap().provider, "anthropic");
        assert_eq!(session.data.lock().unwrap().model, anthropic_default);
        assert!(config_option_has_current_value(&response.config_options, SESSION_CONFIG_PROVIDER_ID, "anthropic"));
        assert!(config_option_has_current_value(
            &response.config_options,
            SESSION_CONFIG_MODEL_ID,
            anthropic_default.as_str()
        ));
    }

    #[tokio::test]
    async fn set_session_config_option_updates_model_for_provider() {
        let temp = TempDir::new().unwrap();
        let (agent, session_id) = build_agent_with_session(temp.path()).await;

        let response = agent
            .set_session_config_option(SetSessionConfigOptionRequest::new(
                session_id.clone(),
                SESSION_CONFIG_MODEL_ID,
                "gpt-5.6-luna",
            ))
            .await
            .unwrap();

        let session = agent.session_handle(&session_id).unwrap().unwrap();
        assert_eq!(session.data.lock().unwrap().provider, "openai");
        assert_session_config_option_is_current(
            &session.data.lock().unwrap().model,
            &response.config_options,
            crate::acp::SessionConfigId::new(SESSION_CONFIG_MODEL_ID),
            crate::acp::SessionConfigValueId::new("gpt-5.6-luna"),
        );
    }
}
