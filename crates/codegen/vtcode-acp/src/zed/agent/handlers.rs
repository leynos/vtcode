//! SACP `AgentToClient` handler registration for `ZedAgent`.
//!
//! The bridge translates SACP request/notification handlers into calls on
//! the existing [`ZedAgent`] methods. The methods themselves are mostly
//! preserved from the pre-1.0.0 trait-based implementation; the only thing
//! that changed is the wiring layer (this file) and the connection storage
//! ([`crate::zed::connection::ConnectionHandle`]).
//!
//! ## Spawn pattern
//!
//! [`acp::PromptRequest`] is the one handler that needs to
//! drive the SACP event loop forward *while* it runs. Per the SACP docs we
//! therefore use [`ConnectionTo::spawn`] from inside the handler closure,
//! and the actual prompt logic runs in the spawned task. The spawned task
//! has full access to `block_task()` and the agent's client-side helpers.
//!
//! All other handlers are quick and can be served synchronously without
//! `spawn`.
//!
//! ## Connection access
//!
//! The canonical `ConnectionHandle` is stashed in
//! [`crate::register_acp_connection`] by `run_acp_agent` before the
//! SACP event loop starts. Handlers reach the same handle through the
//! agent's `self.client()` accessor — they must **not** re-wrap the
//! per-handler `cx` into a new `ConnectionHandle` or they will race
//! each other on the global `Mutex<Option<Arc<ConnectionHandle>>>`
//! inside the agent.

use super::super::constants::*;
use super::super::helpers::{agent_implementation_info, text_chunk};
use super::super::types::{PlanProgress, ToolRuntime};
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, InitializeRequest, InitializeResponse,
    LoadSessionRequest, LoadSessionResponse, NewSessionRequest, NewSessionResponse, PromptRequest, PromptResponse,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_notification,
    on_receive_request,
};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, info, warn};
use vtcode_core::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::factory::ProviderConfig;
use vtcode_core::llm::factory::create_provider_with_config;
use vtcode_core::llm::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStreamEvent, Message};
use vtcode_core::retry::RetryPolicyCoreExt;

use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderRequestRuntime};

struct TurnGuard {
    thread: ThreadRuntimeHandle,
}

impl TurnGuard {
    fn begin(thread: ThreadRuntimeHandle) -> Result<Self, SdkError> {
        let _submission_id = thread.begin_turn().map_err(|error| {
            SdkError::internal_error().data(json!({ "reason": "turn_in_progress", "detail": error.to_string() }))
        })?;
        Ok(Self { thread })
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.thread.finish_turn();
    }
}

#[derive(Debug)]
enum ProviderCallError {
    Cancelled,
    Failed(String),
}

impl From<ProviderAdmissionError> for ProviderCallError {
    fn from(error: ProviderAdmissionError) -> Self {
        match error {
            ProviderAdmissionError::Cancelled => Self::Cancelled,
            other => Self::Failed(other.to_string()),
        }
    }
}

async fn cancellable_backoff(
    delay: std::time::Duration,
    cancellation: &super::super::types::SessionCancellation,
) -> Result<(), ProviderCallError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(ProviderCallError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

async fn generate_with_retry(
    provider: &dyn LLMProvider,
    request: LLMRequest,
    runtime: &ProviderRequestRuntime,
    cancellation: &super::super::types::SessionCancellation,
) -> Result<LLMResponse, ProviderCallError> {
    let policy = runtime.retry_policy();
    let mut attempt_index = 0;

    loop {
        let permit = runtime.acquire(cancellation).await?;
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(ProviderCallError::Cancelled),
            result = provider.generate(request.clone()) => result,
        };
        drop(permit);

        match result {
            Ok(response) => return Ok(response),
            Err(error) => {
                let decision = policy.decision_for_llm_error(&error, attempt_index);
                if !decision.retryable {
                    return Err(ProviderCallError::Failed(error.to_string()));
                }
                let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                info!(
                    provider = runtime.provider_name(),
                    next_attempt = attempt_index + 2,
                    ?delay,
                    "Retrying transient ACP provider request"
                );
                cancellable_backoff(delay, cancellation).await?;
                attempt_index = attempt_index.saturating_add(1);
            }
        }
    }
}

fn response_reasoning_update(response: &LLMResponse) -> Option<acp::SessionUpdate> {
    response
        .reasoning
        .as_deref()
        .filter(|reasoning| !reasoning.is_empty())
        .map(|reasoning| acp::SessionUpdate::AgentThoughtChunk(text_chunk(reasoning.to_string())))
}

async fn emit_response_reasoning(agent: &ZedAgent, session_id: &acp::SessionId, response: &LLMResponse) {
    let has_tool_calls = response.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty());
    let Some(update) = response_reasoning_update(response) else {
        debug!(
            %session_id,
            has_tool_calls,
            "Provider response did not include exposed reasoning for ACP"
        );
        return;
    };

    debug!(
        %session_id,
        reasoning_bytes = response.reasoning.as_ref().map_or(0, String::len),
        has_tool_calls,
        "Sending provider reasoning to ACP client"
    );
    if let Err(error) = agent.send_update(session_id, update).await {
        warn!(%session_id, %error, "Failed to send provider reasoning to ACP client");
    }
}

/// Register every SACP `AgentToClient` request/notification handler that the
/// vtcode bridge implements. The agent must be `Send + Sync + 'static` so
/// that the SACP `Builder` can move the handlers onto its background task.
pub fn install_handlers<H, R>(
    builder: Builder<Agent, H, R>,
    agent: Arc<ZedAgent>,
) -> Builder<Agent, impl HandleDispatchFrom<Client>, R>
where
    H: HandleDispatchFrom<Client>,
    R: RunWithConnectionTo<Client>,
{
    builder
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: InitializeRequest, request_cx: Responder<InitializeResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_initialize(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |_req: AuthenticateRequest, request_cx: Responder<AuthenticateResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_authenticate(agent, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: NewSessionRequest, request_cx: Responder<NewSessionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_new_session(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: LoadSessionRequest, request_cx: Responder<LoadSessionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_load_session(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: SetSessionConfigOptionRequest, request_cx: Responder<SetSessionConfigOptionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_set_session_config_option(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: PromptRequest, request_cx: Responder<PromptResponse>, cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_prompt(agent, req, request_cx, cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_notification(
            {
                let agent = Arc::clone(&agent);
                move |notif: CancelNotification, _cx: ConnectionTo<Client>| {
                    let agent = Arc::clone(&agent);
                    async move { handle_cancel(agent, notif).await }
                }
            },
            on_receive_notification!(),
        )
}

async fn handle_initialize(
    agent: Arc<ZedAgent>,
    args: InitializeRequest,
    request_cx: Responder<InitializeResponse>,
) -> Result<(), SdkError> {
    let caps = args.client_capabilities.clone();
    if let Ok(mut guard) = agent.client_capabilities.lock() {
        *guard = Some(caps);
    }
    if args.protocol_version != acp::ProtocolVersion::V1 {
        warn!(
            requested = %args.protocol_version,
            "{}",
            INITIALIZE_VERSION_MISMATCH_LOG
        );
    }
    let mut capabilities = acp::AgentCapabilities::default();
    capabilities.prompt_capabilities.embedded_context = true;
    capabilities.prompt_capabilities.image = true;
    capabilities.prompt_capabilities.audio = true;
    capabilities.mcp_capabilities.http = true;
    capabilities.mcp_capabilities.sse = false;
    capabilities.load_session = true;

    let auth_methods = build_auth_methods();
    let response = InitializeResponse::new(acp::ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(agent_implementation_info(agent.title()))
        .auth_methods(auth_methods);
    request_cx.respond(response)
}

fn build_auth_methods() -> Vec<acp::AuthMethod> {
    let mut methods = vec![
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openai", "OpenAI OAuth")
                .description("Authenticate with OpenAI via OAuth 2.0 with PKCE"),
        ),
        acp::AuthMethod::Agent(
            acp::AuthMethodAgent::new("oauth-openrouter", "OpenRouter OAuth")
                .description("Authenticate with OpenRouter via OAuth 2.0 with PKCE"),
        ),
    ];
    methods.push(acp::AuthMethod::Terminal(
        acp::AuthMethodTerminal::new("terminal-login", "Terminal Login")
            .description("Interactive terminal-based authentication via vtcode login command")
            .args(vec!["login".to_string()]),
    ));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new("env-api-keys", "API Key", env_api_keys())));
    methods.push(acp::AuthMethod::EnvVar(acp::AuthMethodEnvVar::new(
        "env-base-urls",
        "API Base URL",
        env_base_urls(),
    )));
    methods
}

fn env_api_keys() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str, bool)] = &[
        ("OPENAI_API_KEY", "OpenAI", false),
        ("ANTHROPIC_API_KEY", "Anthropic", false),
        ("GEMINI_API_KEY", "Google Gemini", false),
        ("OPENROUTER_API_KEY", "OpenRouter", false),
        ("DEEPSEEK_API_KEY", "DeepSeek", false),
        ("ZAI_API_KEY", "Z.AI", false),
        ("MOONSHOT_API_KEY", "Moonshot", false),
        ("MINIMAX_API_KEY", "MiniMax", false),
        ("GROQ_API_KEY", "Groq", false),
        ("XAI_API_KEY", "xAI", false),
        ("COHERE_API_KEY", "Cohere", false),
        ("HF_TOKEN", "Hugging Face", false),
        ("MISTRAL_API_KEY", "Mistral", false),
        ("GOOGLE_API_KEY", "Google (alt)", true),
        ("OLLAMA_API_KEY", "Ollama", true),
        ("OLLAMA_API_KEY", "Ollama Cloud", false),
        ("LMSTUDIO_API_KEY", "LM Studio", true),
    ];
    ENTRIES
        .iter()
        .map(|(name, label, optional)| {
            let mut var = acp::AuthEnvVar::new(*name).label(*label);
            if *optional {
                var = var.optional(true);
            }
            var
        })
        .collect()
}

fn env_base_urls() -> Vec<acp::AuthEnvVar> {
    const ENTRIES: &[(&str, &str)] = &[
        ("OPENAI_BASE_URL", "OpenAI"),
        ("ANTHROPIC_BASE_URL", "Anthropic"),
        ("GEMINI_BASE_URL", "Gemini"),
        ("OPENROUTER_BASE_URL", "OpenRouter"),
        ("DEEPSEEK_BASE_URL", "DeepSeek"),
        ("ZAI_BASE_URL", "Z.AI"),
        ("MOONSHOT_BASE_URL", "Moonshot"),
        ("MINIMAX_BASE_URL", "MiniMax"),
        ("XAI_BASE_URL", "xAI"),
        ("HUGGINGFACE_BASE_URL", "Hugging Face"),
        ("OLLAMA_BASE_URL", "Ollama"),
        ("LMSTUDIO_BASE_URL", "LM Studio"),
    ];
    ENTRIES
        .iter()
        .map(|(name, label)| acp::AuthEnvVar::new(*name).label(*label).optional(true))
        .collect()
}

async fn handle_authenticate(
    _agent: Arc<ZedAgent>,
    request_cx: Responder<AuthenticateResponse>,
) -> Result<(), SdkError> {
    request_cx.respond(AuthenticateResponse::default())
}

async fn handle_new_session(
    agent: Arc<ZedAgent>,
    req: NewSessionRequest,
    request_cx: Responder<NewSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.new_session(req).await?;
    request_cx.respond(response)
}

async fn handle_load_session(
    agent: Arc<ZedAgent>,
    req: LoadSessionRequest,
    request_cx: Responder<LoadSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.load_session(req).await?;
    request_cx.respond(response)
}

async fn handle_set_session_config_option(
    agent: Arc<ZedAgent>,
    req: SetSessionConfigOptionRequest,
    request_cx: Responder<SetSessionConfigOptionResponse>,
) -> Result<(), SdkError> {
    let response = agent.set_session_config_option(req).await?;
    request_cx.respond(response)
}

async fn handle_cancel(agent: Arc<ZedAgent>, notif: CancelNotification) -> Result<(), SdkError> {
    if let Some(session) = agent.session_handle(&notif.session_id) {
        session.cancellation.cancel();
    }
    Ok(())
}

async fn handle_prompt(
    agent: Arc<ZedAgent>,
    req: PromptRequest,
    request_cx: Responder<PromptResponse>,
    cx: ConnectionTo<Client>,
) -> Result<(), SdkError> {
    // The prompt handler drives several SACP RPCs (`fs/read_text_file`,
    // `terminal/create`, `session/request_permission`) from inside the
    // prompt loop. Those would deadlock if called directly on the
    // dispatch loop's task, so we spawn the work onto a child task that
    // is allowed to use `block_task()`. The canonical `ConnectionHandle`
    // was registered globally by `run_acp_agent` — we do not re-wrap
    // `cx` into a new handle here, that would race with concurrent
    // prompts on the agent's internal `Mutex<Option<Arc<ConnectionHandle>>>`.
    cx.spawn({
        let agent = Arc::clone(&agent);
        async move {
            let result = run_prompt(agent, req).await;
            if let Err(error) = &result {
                warn!(%error, "ACP prompt failed");
            }
            if let Err(error) = request_cx.respond_with_result(result) {
                warn!(%error, "Failed to send prompt response");
            }
            Ok(())
        }
    })
    .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
    Ok(())
}

async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
    let Some(session) = agent.session_handle(&args.session_id) else {
        return Err(SdkError::invalid_params().data(json!({ "reason": "unknown_session" })));
    };

    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    let _turn_guard = TurnGuard::begin(thread)?;
    agent.local_tool_registry.safety_gateway().start_turn();
    session.cancellation.reset();

    let user_message = tokio::select! {
        () = session.cancellation.cancelled() => return Ok(PromptResponse::new(acp::StopReason::Cancelled)),
        result = agent.resolve_prompt(&args.session_id, &args.prompt) => result?,
    };

    agent.push_message(&session, Message::user(user_message.clone()));

    let (session_provider_name, session_model, session_reasoning_effort) = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        (data.provider.clone(), data.model.clone(), data.reasoning_effort)
    };

    let session_api_key = resolve_api_key_for_provider(&agent, &session_provider_name);
    let mut provider_timeouts = agent.provider_timeouts.clone();
    provider_timeouts.default_ceiling_seconds = provider_timeouts.streaming_ceiling_seconds;
    let provider = create_provider_with_config(
        &session_provider_name,
        ProviderConfig {
            api_key: Some(session_api_key),
            openai_chatgpt_auth: if session_provider_name.eq_ignore_ascii_case("openai") {
                agent.config.openai_chatgpt_auth.clone()
            } else {
                None
            },
            copilot_auth: None,
            base_url: None,
            model: Some(session_model.clone()),
            prompt_cache: Some(agent.config.prompt_cache.clone()),
            timeouts: Some(provider_timeouts),
            openai: None,
            anthropic: None,
            model_behavior: agent.config.model_behavior.clone(),
            workspace_root: Some(agent.config.workspace.clone()),
        },
    )
    .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

    let supports_streaming = provider.supports_streaming();
    let reasoning_effort = if provider.supports_reasoning_effort(&session_model) {
        Some(session_reasoning_effort)
    } else {
        None
    };

    let stop_reason: acp::StopReason;
    let mut assistant_message = String::with_capacity(4096);
    let client_supports_read_text_file = agent.client_supports_read_text_file();
    let provider_supports_tools = provider.supports_tools(&session_model);
    let mut primary_agent = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        data.primary_agent.clone()
    };
    let availability = agent.tool_availability(provider_supports_tools, client_supports_read_text_file);
    let mut enabled_tools = Vec::with_capacity(5);
    for (tool, runtime) in availability {
        if matches!(runtime, ToolRuntime::Enabled) {
            enabled_tools.push(tool);
        }
    }

    let mut has_local_tools = agent.local_tools_available(&primary_agent);
    let mut tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
    let mut tool_definitions = agent
        .tool_definitions(provider_supports_tools, &enabled_tools, &primary_agent)
        .map(Arc::new);
    let mut messages = agent.resolved_messages(&session);
    if let Some(controller) = agent.local_tool_registry.subagent_controller() {
        controller.set_parent_session_id(args.session_id.to_string()).await;
        controller.set_parent_messages(&messages).await;
        drop(controller.set_turn_delegation_hints_from_input(&user_message).await);
    }
    let allow_streaming = supports_streaming && !tools_allowed;
    let provider_runtime = agent.provider_runtime.for_provider(&session_provider_name);

    let mut plan = PlanProgress::new(tools_allowed);
    if plan.has_entries() {
        drop(agent.send_plan_update(&args.session_id, &plan).await);
        if plan.complete_analysis() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
    }

    if allow_streaming {
        let request = LLMRequest {
            messages: Arc::new(messages.clone()),
            model: session_model.clone(),
            stream: true,
            tools: tool_definitions,
            tool_choice: agent.tool_choice(tools_allowed),
            reasoning_effort,
            ..Default::default()
        };

        let policy = provider_runtime.retry_policy();
        let mut attempt_index = 0u32;
        let mut emitted_output = false;

        'stream_attempts: loop {
            let permit = match provider_runtime.acquire(&session.cancellation).await {
                Ok(permit) => permit,
                Err(ProviderAdmissionError::Cancelled) => {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                Err(error) => return Err(SdkError::internal_error().data(error.to_string())),
            };
            let stream_result = tokio::select! {
                () = session.cancellation.cancelled() => {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                result = provider.stream(request.clone()) => result,
            };
            let mut stream = match stream_result {
                Ok(stream) => stream,
                Err(error) => {
                    drop(permit);
                    let decision = policy.decision_for_llm_error(&error, attempt_index);
                    if !decision.retryable {
                        return Err(SdkError::internal_error().data(error.to_string()));
                    }
                    let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                    info!(
                        provider = provider_runtime.provider_name(),
                        next_attempt = attempt_index + 2,
                        ?delay,
                        "Retrying transient ACP provider stream"
                    );
                    match cancellable_backoff(delay, &session.cancellation).await {
                        Ok(()) => {
                            attempt_index = attempt_index.saturating_add(1);
                            continue;
                        }
                        Err(ProviderCallError::Cancelled) => {
                            stop_reason = acp::StopReason::Cancelled;
                            break;
                        }
                        Err(ProviderCallError::Failed(error)) => {
                            return Err(SdkError::internal_error().data(error));
                        }
                    }
                }
            };

            drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);

            loop {
                let event = tokio::select! {
                    () = session.cancellation.cancelled() => {
                        stop_reason = acp::StopReason::Cancelled;
                        break 'stream_attempts;
                    }
                    event = stream.next() => event,
                };
                let Some(event) = event else {
                    let error = LLMError::Network {
                        message: "provider stream ended before a completion event".to_string(),
                        metadata: None,
                    };
                    if emitted_output {
                        return Err(SdkError::internal_error().data(error.to_string()));
                    }
                    drop(stream);
                    drop(permit);
                    let decision = policy.decision_for_llm_error(&error, attempt_index);
                    if !decision.retryable {
                        return Err(SdkError::internal_error().data(error.to_string()));
                    }
                    let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                    info!(
                        provider = provider_runtime.provider_name(),
                        next_attempt = attempt_index + 2,
                        ?delay,
                        "Retrying ACP provider stream that ended before completion"
                    );
                    match cancellable_backoff(delay, &session.cancellation).await {
                        Ok(()) => {
                            attempt_index = attempt_index.saturating_add(1);
                            continue 'stream_attempts;
                        }
                        Err(ProviderCallError::Cancelled) => {
                            stop_reason = acp::StopReason::Cancelled;
                            break 'stream_attempts;
                        }
                        Err(ProviderCallError::Failed(error)) => {
                            return Err(SdkError::internal_error().data(error));
                        }
                    }
                };
                let event = match event {
                    Ok(event) => event,
                    Err(error) if !emitted_output => {
                        drop(stream);
                        drop(permit);
                        let decision = policy.decision_for_llm_error(&error, attempt_index);
                        if !decision.retryable {
                            return Err(SdkError::internal_error().data(error.to_string()));
                        }
                        let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                        info!(
                            provider = provider_runtime.provider_name(),
                            next_attempt = attempt_index + 2,
                            ?delay,
                            "Retrying transient ACP provider stream before output"
                        );
                        match cancellable_backoff(delay, &session.cancellation).await {
                            Ok(()) => {
                                attempt_index = attempt_index.saturating_add(1);
                                continue 'stream_attempts;
                            }
                            Err(ProviderCallError::Cancelled) => {
                                stop_reason = acp::StopReason::Cancelled;
                                break 'stream_attempts;
                            }
                            Err(ProviderCallError::Failed(error)) => {
                                return Err(SdkError::internal_error().data(error));
                            }
                        }
                    }
                    Err(error) => return Err(SdkError::internal_error().data(error.to_string())),
                };

                match event {
                    LLMStreamEvent::Token { delta } => {
                        if !delta.is_empty() {
                            emitted_output = true;
                            assistant_message.push_str(&delta);
                            let chunk = text_chunk(delta);
                            drop(
                                agent
                                    .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                                    .await,
                            );
                        }
                    }
                    LLMStreamEvent::Reasoning { delta } => {
                        if !delta.is_empty() {
                            emitted_output = true;
                            let chunk = text_chunk(delta);
                            drop(
                                agent
                                    .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                                    .await,
                            );
                        }
                    }
                    LLMStreamEvent::ReasoningStage { .. } => {}
                    LLMStreamEvent::ReasoningSignature { .. } => {}
                    LLMStreamEvent::Completed { response } => {
                        if assistant_message.is_empty()
                            && let Some(content) = response.content
                        {
                            if !content.is_empty() {
                                let chunk = text_chunk(content.clone());
                                drop(
                                    agent
                                        .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                                        .await,
                                );
                            }
                            assistant_message.push_str(&content);
                        }

                        if let Some(reasoning) = response.reasoning.filter(|reasoning| !reasoning.is_empty()) {
                            let chunk = text_chunk(reasoning);
                            drop(
                                agent
                                    .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                                    .await,
                            );
                        }

                        stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
                        break 'stream_attempts;
                    }
                }
            }
        }
    } else {
        let mut tool_loop_count = 0usize;
        loop {
            if session.cancellation.is_cancelled() {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            let request = LLMRequest {
                messages: Arc::new(messages.clone()),
                model: session_model.clone(),
                tools: tool_definitions.clone(),
                tool_choice: agent.tool_choice(tools_allowed),
                reasoning_effort,
                ..Default::default()
            };

            let response =
                match generate_with_retry(provider.as_ref(), request, &provider_runtime, &session.cancellation).await {
                    Ok(response) => response,
                    Err(ProviderCallError::Cancelled) => {
                        stop_reason = acp::StopReason::Cancelled;
                        break;
                    }
                    Err(ProviderCallError::Failed(error)) => {
                        return Err(SdkError::internal_error().data(error));
                    }
                };

            if session.cancellation.is_cancelled() {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            emit_response_reasoning(&agent, &args.session_id, &response).await;
            if session.cancellation.is_cancelled() {
                stop_reason = acp::StopReason::Cancelled;
                break;
            }

            if tools_allowed && let Some(tool_calls) = response.tool_calls.clone().filter(|calls| !calls.is_empty()) {
                if agent.tool_loop_limit_reached(tool_loop_count) {
                    let message = agent.tool_loop_limit_message();
                    drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);
                    drop(
                        agent
                            .send_update(
                                &args.session_id,
                                acp::SessionUpdate::AgentMessageChunk(text_chunk(message.clone())),
                            )
                            .await,
                    );
                    assistant_message = message;
                    stop_reason = acp::StopReason::EndTurn;
                    break;
                }
                tool_loop_count = tool_loop_count.saturating_add(1);
                if plan.start_context() {
                    drop(agent.send_plan_update(&args.session_id, &plan).await);
                }
                agent.push_message(
                    &session,
                    Message::assistant_with_tools(response.content.clone().unwrap_or_default(), tool_calls.clone()),
                );
                if let Some(controller) = agent.local_tool_registry.subagent_controller() {
                    controller.set_parent_session_id(args.session_id.to_string()).await;
                    controller.set_parent_messages(&agent.resolved_messages(&session)).await;
                }
                let tool_results = match agent.execute_tool_calls(&session, &args.session_id, &tool_calls).await {
                    Ok(results) => results,
                    Err(error) => {
                        warn!(%error, "Tool execution failed");
                        for call in &tool_calls {
                            agent.push_message(
                                &session,
                                Message::tool_response(
                                    call.id.clone(),
                                    format!("Tool execution was interrupted: {error}"),
                                ),
                            );
                        }
                        return Err(error);
                    }
                };
                if plan.complete_context() {
                    drop(agent.send_plan_update(&args.session_id, &plan).await);
                }
                for result in tool_results {
                    agent.push_message(&session, Message::tool_response(result.tool_call_id, result.llm_response));
                }
                if session.cancellation.is_cancelled() {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                messages = agent.resolved_messages(&session);
                primary_agent = {
                    let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
                    data.primary_agent.clone()
                };
                has_local_tools = agent.local_tools_available(&primary_agent);
                tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
                tool_definitions = agent
                    .tool_definitions(provider_supports_tools, &enabled_tools, &primary_agent)
                    .map(Arc::new);
                continue;
            }

            if let Some(content) = &response.content {
                if !content.is_empty() {
                    drop(agent.advance_plan_to_response(&args.session_id, &mut plan).await);
                    if session.cancellation.is_cancelled() {
                        stop_reason = acp::StopReason::Cancelled;
                        break;
                    }
                    let chunk = text_chunk(content.clone());
                    drop(
                        agent
                            .send_update(&args.session_id, acp::SessionUpdate::AgentMessageChunk(chunk))
                            .await,
                    );
                }
                assistant_message = content.clone();
            }

            stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
            break;
        }
    }

    if stop_reason != acp::StopReason::Cancelled && !assistant_message.is_empty() {
        agent.push_message(&session, Message::assistant(assistant_message));
    }

    if stop_reason != acp::StopReason::Cancelled {
        if plan.complete_context() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
        if plan.complete_response() {
            drop(agent.send_plan_update(&args.session_id, &plan).await);
        }
    }

    Ok(PromptResponse::new(stop_reason))
}

fn resolve_api_key_for_provider(agent: &ZedAgent, provider: &str) -> String {
    if provider.eq_ignore_ascii_case(&agent.config.provider) && !agent.config.api_key.is_empty() {
        return agent.config.api_key.clone();
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), agent.credential_storage_mode).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
    use vtcode_core::core::threads::{ThreadBootstrap, ThreadManager};
    use vtcode_core::llm::provider::LLMError;

    use super::*;

    #[test]
    fn failed_turn_releases_session_and_preserves_gathered_context() {
        let thread = ThreadManager::new().start_thread_with_identifier("acp-recovery", ThreadBootstrap::new(None));
        thread.append_message(Message::user("original request".to_string()));

        {
            let _guard = TurnGuard::begin(thread.clone()).expect("first turn should begin");
            thread.append_message(Message::assistant("context gathered from a file read".to_string()));
        }

        let messages = thread.messages();
        assert_eq!(messages.len(), 2, "failed turn history must remain available to continue");
        let next_turn = thread.begin_turn().expect("failed turn must release the in-flight marker");
        drop(next_turn);
        thread.finish_turn();
    }

    #[test]
    fn concurrent_prompt_is_rejected_without_mutating_history() {
        let thread = ThreadManager::new().start_thread_with_identifier("acp-serial", ThreadBootstrap::new(None));
        let _guard = TurnGuard::begin(thread.clone()).expect("first turn should begin");

        assert!(TurnGuard::begin(thread.clone()).is_err());
        assert!(thread.messages().is_empty());
    }

    #[test]
    fn reasoning_remains_visible_when_response_requests_a_tool() {
        let response = LLMResponse {
            reasoning: Some("Inspect the call graph before editing.".to_string()),
            tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall {
                id: "call-1".to_string(),
                call_type: "function".to_string(),
                function: None,
                text: Some("read_file".to_string()),
                thought_signature: None,
            }]),
            ..LLMResponse::default()
        };

        let Some(acp::SessionUpdate::AgentThoughtChunk(chunk)) = response_reasoning_update(&response) else {
            panic!("reasoning should produce an ACP thought update");
        };
        let acp::ContentBlock::Text(text) = chunk.content else {
            panic!("reasoning update should contain text");
        };
        assert_eq!(text.text, "Inspect the call graph before editing.");
    }

    struct FlakyProvider {
        attempts: AtomicUsize,
        network_failures: usize,
        invalid_request: bool,
    }

    #[async_trait]
    impl LLMProvider for FlakyProvider {
        fn name(&self) -> &str {
            "retry-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["test-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if self.invalid_request {
                return Err(LLMError::InvalidRequest { message: "bad request".to_string(), metadata: None });
            }
            if attempt < self.network_failures {
                return Err(LLMError::Network {
                    message: "temporary disconnect".to_string(),
                    metadata: None,
                });
            }
            Ok(LLMResponse::new("test-model", "recovered"))
        }
    }

    fn retry_runtime() -> ProviderRequestRuntime {
        let provider = CustomProviderConfig {
            name: "retry-test".to_string(),
            request_policy: CustomProviderRequestPolicyConfig {
                max_retries: 2,
                retry_initial_backoff_ms: 1,
                retry_max_backoff_ms: 2,
                retry_jitter: false,
                ..CustomProviderRequestPolicyConfig::default()
            },
            ..CustomProviderConfig::default()
        };
        super::super::super::provider_runtime::ProviderRuntimeRegistry::new(&[provider]).for_provider("retry-test")
    }

    #[tokio::test]
    async fn transient_network_failure_retries_and_recovers() {
        let provider = FlakyProvider {
            attempts: AtomicUsize::new(0),
            network_failures: 1,
            invalid_request: false,
        };

        let response = generate_with_retry(
            &provider,
            LLMRequest::default(),
            &retry_runtime(),
            &super::super::super::types::SessionCancellation::default(),
        )
        .await
        .expect("transient request should recover");

        assert_eq!(response.content.as_deref(), Some("recovered"));
        assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn invalid_request_is_not_retried() {
        let provider = FlakyProvider {
            attempts: AtomicUsize::new(0),
            network_failures: 0,
            invalid_request: true,
        };

        let result = generate_with_retry(
            &provider,
            LLMRequest::default(),
            &retry_runtime(),
            &super::super::super::types::SessionCancellation::default(),
        )
        .await;

        assert!(matches!(result, Err(ProviderCallError::Failed(_))));
        assert_eq!(provider.attempts.load(Ordering::SeqCst), 1);
    }
}
