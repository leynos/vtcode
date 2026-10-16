//! Builds provider and tool context for an ACP prompt before turn execution.
use super::super::provider::create_prompt_provider;
use super::*;
use std::path::Path;
use vtcode_config::api_keys::resolve_credential_with_mode;

pub(super) fn create_prompt_provider_context(input: &PromptInput) -> Result<PromptProviderContext, SdkError> {
    let PromptInput { agent, args, session, .. } = input;
    let (provider_name, model, configured_reasoning_effort) = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        (data.provider.clone(), data.model.clone(), data.reasoning_effort)
    };
    let runtime = agent.provider_runtime.for_provider(&provider_name);
    let mut timeouts = agent.provider_timeouts.clone();
    runtime.apply_http_timeouts(&mut timeouts);
    let workspace_root = session
        .workspace_runtime()
        .map_err(|error| {
            session_access_error(&args.session_id, "reading workspace runtime for provider configuration", error)
        })?
        .map_or_else(|| agent.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
    let openai_chatgpt_auth = if provider_name.eq_ignore_ascii_case("openai") {
        agent.config.openai_chatgpt_auth.clone()
    } else {
        None
    };
    let custom_provider = agent
        .workspace_runtime_config
        .custom_providers
        .iter()
        .find(|provider| provider.name.eq_ignore_ascii_case(&provider_name));
    let configured_api_key_env = custom_provider.map(|provider| provider.resolved_api_key_env()).or_else(|| {
        agent
            .vt_config
            .as_deref()
            .and_then(|config| config.configured_api_key_env(&provider_name))
    });
    let config = ProviderConfig {
        api_key: Some(resolve_api_key_for_provider(
            &provider_name,
            &agent.config.provider,
            &agent.config.api_key,
            configured_api_key_env.as_deref(),
            custom_provider.is_some_and(|provider| provider.uses_command_auth()) || openai_chatgpt_auth.is_some(),
            &workspace_root,
            agent.credential_storage_mode,
        )?),
        openai_chatgpt_auth,
        copilot_auth: None,
        base_url: None,
        model: Some(model.clone()),
        prompt_cache: Some(agent.config.prompt_cache.clone()),
        timeouts: Some(timeouts),
        openai: None,
        anthropic: None,
        model_behaviour: agent.config.model_behaviour.clone(),
        workspace_root: Some(workspace_root),
    };
    let provider = create_prompt_provider(&provider_name, config)
        .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
    let reasoning_effort = provider
        .supports_reasoning_effort(&model)
        .then_some(configured_reasoning_effort);

    Ok(PromptProviderContext {
        supports_streaming: provider.supports_streaming(),
        supports_tools: provider.supports_tools(&model),
        provider,
        runtime,
        model,
        reasoning_effort,
    })
}

pub(super) async fn initialize_prompt_tool_state(
    input: &PromptInput,
    provider: &PromptProviderContext,
) -> Result<PromptToolState, SdkError> {
    let PromptInput { agent, args, session, .. } = input;
    let client_supports_read_text_file = agent
        .client_supports_read_text_file()
        .map_err(|error| session_access_error(&args.session_id, "reading client filesystem capabilities", error))?;
    let primary_agent = session
        .data
        .lock()
        .map_err(|_err| SdkError::internal_error())?
        .primary_agent
        .clone();
    let enabled_tools = agent
        .tool_availability(provider.supports_tools, client_supports_read_text_file)
        .into_iter()
        .filter_map(|(tool, runtime)| matches!(runtime, ToolRuntime::Enabled).then_some(tool))
        .collect();
    let mut tool_state = PromptToolState::new(enabled_tools, primary_agent, provider.supports_tools);
    tool_state.refresh_availability(agent, session, &args.session_id, provider.supports_tools)?;
    tool_state.messages = agent
        .resolved_messages(session)
        .map_err(|error| session_access_error(&args.session_id, "resolving session messages", error))?;
    if maybe_compact_prompt(agent, session, provider, &tool_state).await? {
        tool_state.messages = agent
            .resolved_messages(session)
            .map_err(|error| session_access_error(&args.session_id, "resolving compacted session messages", error))?;
    }
    Ok(tool_state)
}

async fn maybe_compact_prompt(
    agent: &ZedAgent,
    session: &SessionHandle,
    provider: &PromptProviderContext,
    tool_state: &PromptToolState,
) -> Result<bool, SdkError> {
    agent
        .maybe_compact_session(CompactionRequest {
            session,
            provider: provider.provider.as_ref(),
            runtime: &provider.runtime,
            model: &provider.model,
            tools: tool_state.tool_definitions.as_ref(),
        })
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))
}

fn resolve_api_key_for_provider(
    provider: &str,
    launch_provider: &str,
    launch_api_key: &str,
    configured_api_key_env: Option<&str>,
    allow_missing_key: bool,
    workspace_root: &Path,
    storage_mode: vtcode_config::auth::AuthCredentialsStoreMode,
) -> Result<String, SdkError> {
    if provider.eq_ignore_ascii_case(launch_provider) && !launch_api_key.is_empty() {
        return Ok(launch_api_key.to_owned());
    }

    if let Some(key_name) = configured_api_key_env {
        let resolved =
            resolve_credential_with_mode(provider, key_name, Some(workspace_root), storage_mode).map_err(|error| {
                SdkError::internal_error().data(serde_json::json!({
                    "reason": "provider_credential_resolution_failed",
                    "provider": provider,
                    "key_name": key_name,
                    "detail": error.to_string(),
                }))
            })?;
        if let Some(secret) = resolved
            .and_then(|credential| credential.secret)
            .filter(|secret| !secret.trim().is_empty())
        {
            return Ok(secret);
        }
        if allow_missing_key {
            return Ok(String::new());
        }

        return Err(SdkError::internal_error().data(serde_json::json!({
            "reason": "provider_credential_unavailable",
            "provider": provider,
            "key_name": key_name,
            "detail": "No credential was found in the process environment, session workspace, or configured storage.",
        })));
    }

    if allow_missing_key {
        return Ok(String::new());
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), storage_mode).map_err(|error| {
        SdkError::internal_error().data(serde_json::json!({
            "reason": "provider_credential_unavailable",
            "provider": provider,
            "detail": error.to_string(),
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_configured_provider_key_from_session_workspace() {
        let workspace = assert_fs::TempDir::new().expect("create workspace");
        let key_name = format!("CUSTOM_PROVIDER_TEST_{}_API_KEY", uuid::Uuid::new_v4().simple()).to_ascii_uppercase();
        vtcode_config::write_workspace_env_value(workspace.path(), &key_name, "session-workspace-secret")
            .expect("write workspace .env");
        assert_eq!(
            vtcode_config::read_workspace_env_value(workspace.path(), &key_name)
                .expect("read workspace .env")
                .as_deref(),
            Some("session-workspace-secret")
        );
        let api_key = resolve_api_key_for_provider(
            "custom-provider",
            "openai",
            "",
            Some(&key_name),
            false,
            workspace.path(),
            vtcode_config::auth::AuthCredentialsStoreMode::File,
        )
        .expect("resolve configured key from the session workspace");

        assert_eq!(api_key, "session-workspace-secret");
    }

    #[test]
    fn configured_provider_without_a_key_returns_an_sdk_error() {
        let workspace = assert_fs::TempDir::new().expect("create workspace");
        let key_name = format!("VTCODE_ACP_TEST_{}_API_KEY", uuid::Uuid::new_v4().simple());

        let error = resolve_api_key_for_provider(
            "custom-provider",
            "openai",
            "",
            Some(&key_name),
            false,
            workspace.path(),
            vtcode_config::auth::AuthCredentialsStoreMode::File,
        )
        .expect_err("missing required provider credentials should fail");

        assert!(format!("{error:?}").contains("provider_credential_unavailable"));
    }
}
