//! Builds provider and tool context for an ACP prompt before turn execution.
use super::super::provider::create_prompt_provider;
use super::*;

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
    let config = ProviderConfig {
        api_key: Some(resolve_api_key_for_provider(agent, &provider_name)),
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

fn resolve_api_key_for_provider(agent: &ZedAgent, provider: &str) -> String {
    if provider.eq_ignore_ascii_case(&agent.config.provider) && !agent.config.api_key.is_empty() {
        return agent.config.api_key.clone();
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), agent.credential_storage_mode).unwrap_or_default()
}
