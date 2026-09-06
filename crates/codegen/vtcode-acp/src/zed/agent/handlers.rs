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
use super::super::types::{SessionHandle, ToolRuntime};
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use agent_client_protocol::schema::v1::{
    AuthenticateRequest, AuthenticateResponse, CancelNotification, InitializeRequest, InitializeResponse,
    ListSessionsRequest, ListSessionsResponse, LoadSessionRequest, LoadSessionResponse, NewSessionRequest,
    NewSessionResponse, PromptRequest, PromptResponse, ResumeSessionRequest, ResumeSessionResponse,
    SetSessionConfigOptionRequest, SetSessionConfigOptionResponse,
};
use agent_client_protocol::{
    Agent, Builder, Client, ConnectionTo, HandleDispatchFrom, Responder, RunWithConnectionTo, on_receive_notification,
    on_receive_request,
};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tracing::{debug, info, warn};
use vtcode_commons::ansi::strip_ansi_codes;
use vtcode_core::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::factory::ProviderConfig;
use vtcode_core::llm::factory::create_provider_with_config;
use vtcode_core::llm::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStreamEvent, Message};
use vtcode_core::retry::{RetryDecision, RetryPolicyCoreExt};

use crate::zed::provider_runtime::{ProviderAdmissionError, ProviderDeadlinePolicy, ProviderRequestRuntime};

#[cfg(test)]
type PromptProviderFactory = dyn Fn() -> Box<dyn LLMProvider> + Send + Sync;

#[cfg(test)]
struct PromptProviderOverride {
    provider_name: String,
    factory: Arc<PromptProviderFactory>,
}

#[cfg(test)]
static PROMPT_PROVIDER_OVERRIDE: std::sync::LazyLock<std::sync::Mutex<Option<PromptProviderOverride>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

fn create_prompt_provider(provider_name: &str, config: ProviderConfig) -> Result<Box<dyn LLMProvider>, LLMError> {
    #[cfg(test)]
    if let Some(provider) = PROMPT_PROVIDER_OVERRIDE.lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .filter(|provider_override| provider_override.provider_name == provider_name)
            .map(|provider_override| (provider_override.factory)())
    }) {
        return Ok(provider);
    }

    create_provider_with_config(provider_name, config)
}

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
    delay: Duration,
    cancellation: &super::super::types::SessionCancellation,
) -> Result<(), ProviderCallError> {
    tokio::select! {
        () = cancellation.cancelled() => Err(ProviderCallError::Cancelled),
        () = tokio::time::sleep(delay) => Ok(()),
    }
}

async fn sleep_until_optional(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn deadline_after(timeout: Option<Duration>) -> Option<Instant> {
    deadline_from(Instant::now(), timeout)
}

fn deadline_from(started_at: Instant, timeout: Option<Duration>) -> Option<Instant> {
    timeout.map(|timeout| started_at + timeout)
}

fn provider_timeout_error(provider: &str, phase: &str, timeout: Option<Duration>) -> LLMError {
    let duration = timeout.map_or_else(|| "configured deadline".to_string(), |timeout| format!("{timeout:?}"));
    LLMError::Network {
        message: format!("provider '{provider}' exceeded its {phase} timeout ({duration})"),
        metadata: None,
    }
}

fn unix_timestamp_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

struct IncompleteProviderTurn {
    message: Message,
    visible_update: String,
    response: PromptResponse,
}

impl IncompleteProviderTurn {
    fn from_failure(content: &str, reasoning: &str, error: &str) -> Self {
        let sanitized_error = strip_ansi_codes(error).trim().to_string();
        let error_detail = if sanitized_error.is_empty() {
            "The provider did not report any additional details."
        } else {
            sanitized_error.as_str()
        };
        let notice = format!(
            "The provider could not complete this turn. You can retry the prompt.\n\nProvider error: {error_detail}"
        );
        let visible_update = if content.is_empty() {
            notice
        } else {
            format!("\n\n{notice}")
        };
        let message_content = format!("{content}{visible_update}");
        let message = incomplete_assistant_message(&message_content, reasoning, error_detail);
        Self {
            message,
            visible_update,
            response: PromptResponse::new(acp::StopReason::EndTurn),
        }
    }
}

fn incomplete_assistant_message(content: &str, reasoning: &str, error: &str) -> Message {
    let mut message = Message::assistant(content.to_string());
    if !reasoning.is_empty() {
        message.reasoning = Some(reasoning.to_string());
    }
    message.metadata = Some(MessageMetadata::incomplete_llm_response(
        unix_timestamp_millis(),
        message.estimate_tokens(),
        strip_ansi_codes(error).trim(),
    ));
    message
}

async fn persist_session_checkpoint(agent: &ZedAgent, session: &SessionHandle, boundary: &'static str) {
    if let Err(error) = agent.checkpoint_session(session).await {
        warn!(%error, boundary, "Failed to persist ACP session checkpoint");
    }
    session.update_transcript_path().await;
}

async fn finish_failed_provider_turn(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    content: &str,
    reasoning: &str,
    error: &str,
) -> PromptResponse {
    let IncompleteProviderTurn { message, visible_update, response } =
        IncompleteProviderTurn::from_failure(content, reasoning, error);
    drop(
        agent
            .send_update(session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(visible_update)))
            .await,
    );
    agent.push_message(session, message);
    persist_session_checkpoint(agent, session, "incomplete_provider_turn").await;
    warn!(
        provider_error = %strip_ansi_codes(error),
        partial_text_bytes = content.len(),
        partial_reasoning_bytes = reasoning.len(),
        "ACP provider failed to complete the turn"
    );
    response
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamTimeoutPhase {
    FirstToken,
    InterTokenIdle,
    TotalGeneration,
}

async fn sleep_until_stream_deadline(deadline: Option<(StreamTimeoutPhase, Instant)>) -> StreamTimeoutPhase {
    match deadline {
        Some((phase, deadline)) => {
            tokio::time::sleep_until(deadline).await;
            phase
        }
        None => std::future::pending().await,
    }
}

impl StreamTimeoutPhase {
    const fn label(self) -> &'static str {
        match self {
            Self::FirstToken => "time to first token",
            Self::InterTokenIdle => "inter-token idle",
            Self::TotalGeneration => "total generation",
        }
    }

    const fn timeout(self, policy: ProviderDeadlinePolicy) -> Option<Duration> {
        match self {
            Self::FirstToken => policy.first_token,
            Self::InterTokenIdle => policy.stream_idle,
            Self::TotalGeneration => policy.total_generation,
        }
    }
}

struct StreamDeadlineTracker {
    policy: ProviderDeadlinePolicy,
    first_token: Option<Instant>,
    idle: Option<Instant>,
    total: Option<Instant>,
}

struct GenerationTelemetry {
    started_at: Instant,
    first_output_at: Option<Instant>,
    estimated_output_tokens: u64,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProviderErrorTelemetry<'a> {
    code: Option<&'a str>,
    status: Option<u16>,
    detail: Option<&'a str>,
}

fn provider_error_telemetry(error: &LLMError) -> ProviderErrorTelemetry<'_> {
    let LLMError::Network { metadata: Some(metadata), .. } = error else {
        return ProviderErrorTelemetry::default();
    };
    ProviderErrorTelemetry {
        code: metadata.code.as_deref(),
        status: metadata.status,
        detail: metadata.message.as_deref(),
    }
}

impl GenerationTelemetry {
    fn start() -> Self {
        Self {
            started_at: Instant::now(),
            first_output_at: None,
            estimated_output_tokens: 0,
        }
    }

    fn observe_output(&mut self, runtime: &ProviderRequestRuntime, delta: &str, retry_count: u32) {
        let estimated_delta = u64::try_from(delta.chars().count().div_ceil(4)).unwrap_or(u64::MAX);
        self.estimated_output_tokens = self.estimated_output_tokens.saturating_add(estimated_delta);
        if self.first_output_at.is_some() {
            return;
        }

        let now = Instant::now();
        self.first_output_at = Some(now);
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider = runtime.provider_name(),
            time_to_first_token_ms = duration_millis(now.duration_since(self.started_at)),
            retry_count,
            queue_depth = snapshot.queue_depth,
            active_provider_permits = snapshot.active_permits,
            permit_limit = ?snapshot.permit_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider produced its first output"
        );
    }

    fn complete(&self, runtime: &ProviderRequestRuntime, response: &LLMResponse, retry_count: u32, buffered: bool) {
        let elapsed = self.started_at.elapsed();
        let elapsed_ms = duration_millis(elapsed).max(1);
        let (output_tokens, token_count_source) = response
            .usage
            .as_ref()
            .filter(|usage| usage.completion_tokens > 0)
            .map(|usage| (u64::from(usage.completion_tokens), "provider"))
            .unwrap_or((self.estimated_output_tokens, "estimated"));
        let tokens_per_second = output_tokens.saturating_mul(1_000) / elapsed_ms;
        let snapshot = runtime.telemetry_snapshot();
        info!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = elapsed_ms,
            time_to_first_token_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)))
                .unwrap_or(elapsed_ms),
            ttft_observation = if buffered { "buffered_response" } else { "stream_event" },
            output_tokens,
            token_count_source,
            tokens_per_second,
            retry_count,
            queue_depth = snapshot.queue_depth,
            active_provider_permits = snapshot.active_permits,
            permit_limit = ?snapshot.permit_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider generation completed"
        );
    }

    fn failed(
        &self,
        runtime: &ProviderRequestRuntime,
        retry_count: u32,
        retry_disposition: &'static str,
        error: &LLMError,
    ) {
        let snapshot = runtime.telemetry_snapshot();
        let error_telemetry = provider_error_telemetry(error);
        warn!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = duration_millis(self.started_at.elapsed()),
            retry_count,
            max_retries = runtime.retry_policy().max_attempts.saturating_sub(1),
            retry_disposition,
            queue_depth = snapshot.queue_depth,
            active_provider_permits = snapshot.active_permits,
            permit_limit = ?snapshot.permit_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            provider_error_code = ?error_telemetry.code,
            provider_error_status = ?error_telemetry.status,
            provider_error_detail = ?error_telemetry.detail,
            provider_error = %error,
            "ACP provider generation attempt failed"
        );
    }
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn retry_disposition(decision: &RetryDecision) -> &'static str {
    if decision.retryable {
        "retry_scheduled"
    } else if decision.category.is_retryable() {
        "retry_exhausted"
    } else {
        "non_retryable"
    }
}

impl StreamDeadlineTracker {
    fn new(policy: ProviderDeadlinePolicy, started_at: Instant) -> Self {
        Self {
            policy,
            first_token: deadline_from(started_at, policy.first_token),
            idle: None,
            total: deadline_from(started_at, policy.total_generation),
        }
    }

    fn observe_output(&mut self) {
        self.observe_output_at(Instant::now());
    }

    fn observe_output_at(&mut self, observed_at: Instant) {
        self.first_token = None;
        self.idle = deadline_from(observed_at, self.policy.stream_idle);
    }

    fn next(&self) -> Option<(StreamTimeoutPhase, Instant)> {
        [
            self.first_token.map(|deadline| (StreamTimeoutPhase::FirstToken, deadline)),
            self.idle.map(|deadline| (StreamTimeoutPhase::InterTokenIdle, deadline)),
            self.total.map(|deadline| (StreamTimeoutPhase::TotalGeneration, deadline)),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|(_phase, deadline)| *deadline)
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
        let mut telemetry = GenerationTelemetry::start();
        let deadline_policy = runtime.deadline_policy();
        let total_deadline = deadline_after(deadline_policy.total_generation);
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(ProviderCallError::Cancelled),
            () = sleep_until_optional(total_deadline) => {
                Err(provider_timeout_error(
                    runtime.provider_name(),
                    "total generation",
                    deadline_policy.total_generation,
                ))
            }
            result = provider.generate(request.clone()) => result,
        };
        match result {
            Ok(response) => {
                if let Some(content) = response.content.as_deref() {
                    telemetry.observe_output(runtime, content, attempt_index);
                }
                if let Some(reasoning) = response.reasoning.as_deref() {
                    telemetry.observe_output(runtime, reasoning, attempt_index);
                }
                telemetry.complete(runtime, &response, attempt_index, true);
                drop(permit);
                return Ok(response);
            }
            Err(error) => {
                let decision = policy.decision_for_llm_error(&error, attempt_index);
                telemetry.failed(runtime, attempt_index, retry_disposition(&decision), &error);
                drop(permit);
                if !decision.retryable {
                    return Err(ProviderCallError::Failed(error.to_string()));
                }
                let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                info!(
                    provider = runtime.provider_name(),
                    next_attempt = attempt_index + 2,
                    retry_count = attempt_index + 1,
                    max_retries = policy.max_attempts.saturating_sub(1),
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
                move |req: ListSessionsRequest, request_cx: Responder<ListSessionsResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_list_sessions(agent, req, request_cx).await }
                }
            },
            on_receive_request!(),
        )
        .on_receive_request(
            {
                let agent = Arc::clone(&agent);
                move |req: ResumeSessionRequest, request_cx: Responder<ResumeSessionResponse>, _cx| {
                    let agent = Arc::clone(&agent);
                    async move { handle_resume_session(agent, req, request_cx).await }
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
    let mut capabilities = advertised_agent_capabilities();
    capabilities.prompt_capabilities.embedded_context = true;
    capabilities.prompt_capabilities.image = true;
    capabilities.prompt_capabilities.audio = true;
    capabilities.mcp_capabilities.http = true;
    capabilities.mcp_capabilities.sse = false;

    let auth_methods = build_auth_methods();
    let response = InitializeResponse::new(acp::ProtocolVersion::V1)
        .agent_capabilities(capabilities)
        .agent_info(agent_implementation_info(agent.title()))
        .auth_methods(auth_methods);
    request_cx.respond(response)
}

fn advertised_agent_capabilities() -> acp::AgentCapabilities {
    let mut capabilities = acp::AgentCapabilities::default();
    capabilities.load_session = true;
    capabilities.session_capabilities = acp::SessionCapabilities::new()
        .list(acp::SessionListCapabilities::new())
        .resume(acp::SessionResumeCapabilities::new());
    capabilities
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
        ("META_API_KEY", "Meta AI", false),
        ("MODEL_API_KEY", "Meta AI (documented)", false),
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
        ("META_BASE_URL", "Meta AI"),
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

async fn handle_list_sessions(
    agent: Arc<ZedAgent>,
    req: ListSessionsRequest,
    request_cx: Responder<ListSessionsResponse>,
) -> Result<(), SdkError> {
    let response = agent.list_sessions(req).await?;
    request_cx.respond(response)
}

async fn handle_resume_session(
    agent: Arc<ZedAgent>,
    req: ResumeSessionRequest,
    request_cx: Responder<ResumeSessionResponse>,
) -> Result<(), SdkError> {
    let response = agent.resume_session(req).await?;
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

    if let Err(error) = agent.replay_persisted_task_plan(&session, &args.session_id).await {
        warn!(%error, session_id = %args.session_id, "Failed to replay persisted ACP task plan");
    }

    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    let _turn_guard = TurnGuard::begin(thread)?;
    if let Some(runtime) = session.workspace_runtime() {
        runtime.local_tool_registry.safety_gateway().start_turn();
    } else {
        agent.local_tool_registry.safety_gateway().start_turn();
    }
    session.cancellation.reset();

    let user_message = tokio::select! {
        () = session.cancellation.cancelled() => return Ok(PromptResponse::new(acp::StopReason::Cancelled)),
        result = agent.resolve_prompt(&session, &args.session_id, &args.prompt) => result?,
    };

    if let Some(hooks) = session.lifecycle_hooks() {
        let outcome = hooks
            .run_user_prompt_submit(&args.session_id.to_string(), &user_message)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            warn!(level = ?message.level, message = %message.text, "ACP UserPromptSubmit hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                agent.push_message(&session, Message::system(context));
            }
        }
        if !outcome.allow_prompt {
            let reason = outcome
                .block_reason
                .unwrap_or_else(|| "Prompt blocked by lifecycle hook".to_string());
            warn!(%reason, "ACP UserPromptSubmit hook blocked prompt");
            return Ok(PromptResponse::new(acp::StopReason::Refusal));
        }
    }

    agent.push_message(&session, Message::user(user_message.clone()));
    agent.merge_session_acp_meta(&session, args.meta.clone());
    persist_session_checkpoint(&agent, &session, "user_message").await;

    let (session_provider_name, session_model, session_reasoning_effort) = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        (data.provider.clone(), data.model.clone(), data.reasoning_effort)
    };

    let session_api_key = resolve_api_key_for_provider(&agent, &session_provider_name);
    let provider_runtime = agent.provider_runtime.for_provider(&session_provider_name);
    let mut provider_timeouts = agent.provider_timeouts.clone();
    provider_runtime.apply_http_timeouts(&mut provider_timeouts);
    let provider = create_prompt_provider(
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
            workspace_root: Some(
                session
                    .workspace_runtime()
                    .map_or_else(|| agent.config.workspace.clone(), |runtime| runtime.workspace_root.clone()),
            ),
        },
    )
    .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

    let supports_streaming = provider.supports_streaming();
    let reasoning_effort = if provider.supports_reasoning_effort(&session_model) {
        Some(session_reasoning_effort)
    } else {
        None
    };

    let mut stop_reason: acp::StopReason;
    let mut assistant_message = String::with_capacity(4096);
    let mut assistant_reasoning = String::with_capacity(2048);
    let mut stop_hook_active = false;
    let mut stop_hook_checked = false;
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

    let mut has_local_tools = agent.session_local_tools_available(&session, &primary_agent);
    let mut tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
    let mut tool_definitions = agent
        .session_tool_definitions(&session, provider_supports_tools, &enabled_tools, &primary_agent)
        .map(Arc::new);
    let mut messages = agent.resolved_messages(&session);
    if agent
        .maybe_compact_session(
            &session,
            provider.as_ref(),
            &provider_runtime,
            &session_model,
            tool_definitions.as_ref(),
        )
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))?
    {
        messages = agent.resolved_messages(&session);
    }
    if let Some(controller) = agent.session_subagent_controller(&session) {
        controller.set_parent_session_id(args.session_id.to_string()).await;
        controller.set_parent_messages(&messages).await;
        drop(controller.set_turn_delegation_hints_from_input(&user_message).await);
    }
    // Stop hooks need a complete draft before a blocking reason can feed back
    // into the same turn. Other lifecycle hooks do not disable streaming.
    let allow_streaming = should_allow_streaming(supports_streaming, tools_allowed, session.has_stop_hooks());
    if allow_streaming {
        let mut tool_loop_count = 0usize;
        let mut request = LLMRequest {
            messages: Arc::new(messages.clone()),
            model: session_model.clone(),
            stream: true,
            tools: tool_definitions.clone(),
            tool_choice: agent.tool_choice(tools_allowed),
            reasoning_effort,
            ..Default::default()
        };

        let policy = provider_runtime.retry_policy();
        let deadline_policy = provider_runtime.deadline_policy();
        let mut attempt_index = 0u32;
        let mut emitted_output = false;

        'stream_attempts: loop {
            let permit = match provider_runtime.acquire(&session.cancellation).await {
                Ok(permit) => permit,
                Err(ProviderAdmissionError::Cancelled) => {
                    stop_reason = acp::StopReason::Cancelled;
                    break 'stream_attempts;
                }
                Err(error) => return Err(SdkError::internal_error().data(error.to_string())),
            };
            let mut telemetry = GenerationTelemetry::start();
            let mut stream_deadlines = StreamDeadlineTracker::new(deadline_policy, Instant::now());
            let next_deadline = stream_deadlines.next();
            let stream_result = tokio::select! {
                () = session.cancellation.cancelled() => {
                    stop_reason = acp::StopReason::Cancelled;
                    break 'stream_attempts;
                }
                phase = sleep_until_stream_deadline(next_deadline) => {
                    Err(provider_timeout_error(
                        provider_runtime.provider_name(),
                        phase.label(),
                        phase.timeout(deadline_policy),
                    ))
                }
                result = provider.stream(request.clone()) => result,
            };
            let mut stream = match stream_result {
                Ok(stream) => stream,
                Err(error) => {
                    let decision = policy.decision_for_llm_error(&error, attempt_index);
                    telemetry.failed(&provider_runtime, attempt_index, retry_disposition(&decision), &error);
                    drop(permit);
                    if !decision.retryable {
                        return Ok(finish_failed_provider_turn(
                            &agent,
                            &session,
                            &args.session_id,
                            &assistant_message,
                            &assistant_reasoning,
                            &error.to_string(),
                        )
                        .await);
                    }
                    let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                    info!(
                        provider = provider_runtime.provider_name(),
                        next_attempt = attempt_index + 2,
                        retry_count = attempt_index + 1,
                        max_retries = policy.max_attempts.saturating_sub(1),
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
                            break 'stream_attempts;
                        }
                        Err(ProviderCallError::Failed(error)) => {
                            return Ok(finish_failed_provider_turn(
                                &agent,
                                &session,
                                &args.session_id,
                                &assistant_message,
                                &assistant_reasoning,
                                &error,
                            )
                            .await);
                        }
                    }
                }
            };

            loop {
                let next_deadline = stream_deadlines.next();
                let event = tokio::select! {
                    () = session.cancellation.cancelled() => {
                        stop_reason = acp::StopReason::Cancelled;
                        break 'stream_attempts;
                    }
                    phase = sleep_until_stream_deadline(next_deadline) => {
                        Some(Err(provider_timeout_error(
                            provider_runtime.provider_name(),
                            phase.label(),
                            phase.timeout(deadline_policy),
                        )))
                    }
                    event = stream.next() => event,
                };
                let Some(event) = event else {
                    let error = LLMError::Network {
                        message: "provider stream ended before a completion event".to_string(),
                        metadata: None,
                    };
                    if emitted_output {
                        telemetry.failed(&provider_runtime, attempt_index, "partial_output_visible", &error);
                        return Ok(finish_failed_provider_turn(
                            &agent,
                            &session,
                            &args.session_id,
                            &assistant_message,
                            &assistant_reasoning,
                            &error.to_string(),
                        )
                        .await);
                    }
                    drop(stream);
                    let decision = policy.decision_for_llm_error(&error, attempt_index);
                    telemetry.failed(&provider_runtime, attempt_index, retry_disposition(&decision), &error);
                    drop(permit);
                    if !decision.retryable {
                        return Ok(finish_failed_provider_turn(
                            &agent,
                            &session,
                            &args.session_id,
                            &assistant_message,
                            &assistant_reasoning,
                            &error.to_string(),
                        )
                        .await);
                    }
                    let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                    info!(
                        provider = provider_runtime.provider_name(),
                        next_attempt = attempt_index + 2,
                        retry_count = attempt_index + 1,
                        max_retries = policy.max_attempts.saturating_sub(1),
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
                            return Ok(finish_failed_provider_turn(
                                &agent,
                                &session,
                                &args.session_id,
                                &assistant_message,
                                &assistant_reasoning,
                                &error,
                            )
                            .await);
                        }
                    }
                };
                let event = match event {
                    Ok(event) => event,
                    Err(error) if !emitted_output => {
                        drop(stream);
                        let decision = policy.decision_for_llm_error(&error, attempt_index);
                        telemetry.failed(&provider_runtime, attempt_index, retry_disposition(&decision), &error);
                        drop(permit);
                        if !decision.retryable {
                            return Ok(finish_failed_provider_turn(
                                &agent,
                                &session,
                                &args.session_id,
                                &assistant_message,
                                &assistant_reasoning,
                                &error.to_string(),
                            )
                            .await);
                        }
                        let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
                        info!(
                            provider = provider_runtime.provider_name(),
                            next_attempt = attempt_index + 2,
                            retry_count = attempt_index + 1,
                            max_retries = policy.max_attempts.saturating_sub(1),
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
                                return Ok(finish_failed_provider_turn(
                                    &agent,
                                    &session,
                                    &args.session_id,
                                    &assistant_message,
                                    &assistant_reasoning,
                                    &error,
                                )
                                .await);
                            }
                        }
                    }
                    Err(error) => {
                        telemetry.failed(&provider_runtime, attempt_index, "partial_output_visible", &error);
                        return Ok(finish_failed_provider_turn(
                            &agent,
                            &session,
                            &args.session_id,
                            &assistant_message,
                            &assistant_reasoning,
                            &error.to_string(),
                        )
                        .await);
                    }
                };

                match event {
                    LLMStreamEvent::Token { delta } => {
                        if !delta.is_empty() {
                            emitted_output = true;
                            stream_deadlines.observe_output();
                            telemetry.observe_output(&provider_runtime, &delta, attempt_index);
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
                            stream_deadlines.observe_output();
                            telemetry.observe_output(&provider_runtime, &delta, attempt_index);
                            assistant_reasoning.push_str(&delta);
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
                        let response = *response;
                        if telemetry.first_output_at.is_none() {
                            if let Some(content) = response.content.as_deref() {
                                telemetry.observe_output(&provider_runtime, content, attempt_index);
                            }
                            if let Some(reasoning) = response.reasoning.as_deref() {
                                telemetry.observe_output(&provider_runtime, reasoning, attempt_index);
                            }
                        }
                        telemetry.complete(&provider_runtime, &response, attempt_index, false);
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

                        if assistant_reasoning.is_empty()
                            && let Some(reasoning) = response.reasoning.filter(|reasoning| !reasoning.is_empty())
                        {
                            let chunk = text_chunk(reasoning.clone());
                            drop(
                                agent
                                    .send_update(&args.session_id, acp::SessionUpdate::AgentThoughtChunk(chunk))
                                    .await,
                            );
                            assistant_reasoning.push_str(&reasoning);
                        }

                        drop(stream);
                        drop(permit);

                        if tools_allowed
                            && let Some(tool_calls) = response.tool_calls.clone().filter(|calls| !calls.is_empty())
                        {
                            if agent.tool_loop_limit_reached(tool_loop_count) {
                                let message = agent.tool_loop_limit_message();
                                assistant_message = message;
                                stop_reason = acp::StopReason::EndTurn;
                                break 'stream_attempts;
                            }
                            tool_loop_count = tool_loop_count.saturating_add(1);
                            let mut assistant_tool_message =
                                Message::assistant_with_tools(assistant_message.clone(), tool_calls.clone());
                            if !assistant_reasoning.is_empty() {
                                assistant_tool_message.reasoning = Some(assistant_reasoning.clone());
                            }
                            agent.push_message(&session, assistant_tool_message);
                            persist_session_checkpoint(&agent, &session, "assistant_tool_calls").await;
                            if let Some(controller) = agent.session_subagent_controller(&session) {
                                controller.set_parent_session_id(args.session_id.to_string()).await;
                                controller.set_parent_messages(&agent.resolved_messages(&session)).await;
                            }
                            let tool_results =
                                match agent.execute_tool_calls(&session, &args.session_id, &tool_calls).await {
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
                                        persist_session_checkpoint(&agent, &session, "interrupted_tool_results").await;
                                        return Err(error);
                                    }
                                };
                            for result in tool_results {
                                agent.push_message(
                                    &session,
                                    Message::tool_response(result.tool_call_id, result.llm_response),
                                );
                            }
                            persist_session_checkpoint(&agent, &session, "tool_results").await;
                            if session.cancellation.is_cancelled() {
                                stop_reason = acp::StopReason::Cancelled;
                                break 'stream_attempts;
                            }
                            messages = agent.resolved_messages(&session);
                            primary_agent = {
                                let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
                                data.primary_agent.clone()
                            };
                            has_local_tools = agent.session_local_tools_available(&session, &primary_agent);
                            tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
                            tool_definitions = agent
                                .session_tool_definitions(
                                    &session,
                                    provider_supports_tools,
                                    &enabled_tools,
                                    &primary_agent,
                                )
                                .map(Arc::new);
                            if agent
                                .maybe_compact_session(
                                    &session,
                                    provider.as_ref(),
                                    &provider_runtime,
                                    &session_model,
                                    tool_definitions.as_ref(),
                                )
                                .await
                                .map_err(|error| SdkError::internal_error().data(error.to_string()))?
                            {
                                messages = agent.resolved_messages(&session);
                                if let Some(controller) = agent.session_subagent_controller(&session) {
                                    controller.set_parent_messages(&messages).await;
                                }
                            }
                            request = LLMRequest {
                                messages: Arc::new(messages.clone()),
                                model: session_model.clone(),
                                stream: true,
                                tools: tool_definitions.clone(),
                                tool_choice: agent.tool_choice(tools_allowed),
                                reasoning_effort,
                                ..Default::default()
                            };
                            assistant_message.clear();
                            assistant_reasoning.clear();
                            attempt_index = 0;
                            emitted_output = false;
                            continue 'stream_attempts;
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

            if agent
                .maybe_compact_session(
                    &session,
                    provider.as_ref(),
                    &provider_runtime,
                    &session_model,
                    tool_definitions.as_ref(),
                )
                .await
                .map_err(|error| SdkError::internal_error().data(error.to_string()))?
            {
                messages = agent.resolved_messages(&session);
                if let Some(controller) = agent.session_subagent_controller(&session) {
                    controller.set_parent_messages(&messages).await;
                }
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
                        return Ok(finish_failed_provider_turn(
                            &agent,
                            &session,
                            &args.session_id,
                            &assistant_message,
                            &assistant_reasoning,
                            &error,
                        )
                        .await);
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
                    assistant_message = message;
                    stop_reason = acp::StopReason::EndTurn;
                    break;
                }
                tool_loop_count = tool_loop_count.saturating_add(1);
                agent.push_message(
                    &session,
                    Message::assistant_with_tools(response.content.clone().unwrap_or_default(), tool_calls.clone()),
                );
                persist_session_checkpoint(&agent, &session, "assistant_tool_calls").await;
                if let Some(controller) = agent.session_subagent_controller(&session) {
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
                        persist_session_checkpoint(&agent, &session, "interrupted_tool_results").await;
                        return Err(error);
                    }
                };
                for result in tool_results {
                    agent.push_message(&session, Message::tool_response(result.tool_call_id, result.llm_response));
                }
                persist_session_checkpoint(&agent, &session, "tool_results").await;
                if session.cancellation.is_cancelled() {
                    stop_reason = acp::StopReason::Cancelled;
                    break;
                }
                messages = agent.resolved_messages(&session);
                primary_agent = {
                    let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
                    data.primary_agent.clone()
                };
                has_local_tools = agent.session_local_tools_available(&session, &primary_agent);
                tools_allowed = provider_supports_tools && (!enabled_tools.is_empty() || has_local_tools);
                tool_definitions = agent
                    .session_tool_definitions(&session, provider_supports_tools, &enabled_tools, &primary_agent)
                    .map(Arc::new);
                continue;
            }

            if let Some(content) = &response.content {
                if !content.is_empty() {
                    if session.cancellation.is_cancelled() {
                        stop_reason = acp::StopReason::Cancelled;
                        break;
                    }
                }
                assistant_message = content.clone();
            }

            stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
            if stop_reason != acp::StopReason::Cancelled
                && let Some(reason) = agent.run_stop_hook(&session, &assistant_message, stop_hook_active).await?
            {
                stop_hook_active = true;
                agent.push_message(&session, Message::assistant(assistant_message.clone()));
                agent.push_message(&session, Message::system(reason));
                messages = agent.resolved_messages(&session);
                assistant_message.clear();
                assistant_reasoning.clear();
                stop_hook_checked = false;
                continue;
            } else if stop_reason != acp::StopReason::Cancelled {
                stop_hook_checked = true;
            }
            break;
        }
    }

    if stop_reason != acp::StopReason::Cancelled {
        if !stop_hook_checked
            && let Some(reason) = agent.run_stop_hook(&session, &assistant_message, stop_hook_active).await?
        {
            warn!(%reason, "ACP Stop hook blocked final turn");
            agent.push_message(&session, Message::system(reason));
            persist_session_checkpoint(&agent, &session, "stop_hook_blocked").await;
            return Ok(PromptResponse::new(acp::StopReason::Refusal));
        }
        if !assistant_message.is_empty() {
            if should_emit_buffered_final_chunk(allow_streaming) {
                drop(
                    agent
                        .send_update(
                            &args.session_id,
                            acp::SessionUpdate::AgentMessageChunk(text_chunk(assistant_message.clone())),
                        )
                        .await,
                );
            }
            let mut completed_message = Message::assistant(assistant_message);
            if !assistant_reasoning.is_empty() {
                completed_message.reasoning = Some(assistant_reasoning);
            }
            agent.push_message(&session, completed_message);
            persist_session_checkpoint(&agent, &session, "assistant_response").await;
        }
    }

    Ok(PromptResponse::new(stop_reason))
}

impl ZedAgent {
    async fn run_stop_hook(
        &self,
        session: &SessionHandle,
        assistant_message: &str,
        stop_hook_active: bool,
    ) -> Result<Option<String>, SdkError> {
        let Some(hooks) = session.lifecycle_hooks() else {
            return Ok(None);
        };
        let outcome = hooks
            .run_stop(assistant_message, stop_hook_active)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            warn!(level = ?message.level, message = %message.text, "ACP Stop hook");
        }
        Ok(outcome.block_reason)
    }
}

fn resolve_api_key_for_provider(agent: &ZedAgent, provider: &str) -> String {
    if provider.eq_ignore_ascii_case(&agent.config.provider) && !agent.config.api_key.is_empty() {
        return agent.config.api_key.clone();
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), agent.credential_storage_mode).unwrap_or_default()
}

/// Streaming is safe unless a Stop hook needs to inspect the complete draft.
/// Other lifecycle hooks do not affect whether ACP can receive streamed text.
fn should_allow_streaming(supports_streaming: bool, _tools_allowed: bool, has_stop_hooks: bool) -> bool {
    supports_streaming && !has_stop_hooks
}

fn should_emit_buffered_final_chunk(allow_streaming: bool) -> bool {
    !allow_streaming
}

#[cfg(test)]
mod tests {
    use agent_client_protocol::{Channel, on_receive_notification, on_receive_request};
    use assert_fs::TempDir;
    use async_trait::async_trait;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Mutex as AsyncMutex, mpsc};
    use vtcode_config::SubagentDiscoveryInput;
    use vtcode_config::auth::AuthCredentialsStoreMode;
    use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::types::{
        AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
    };
    use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig};
    use vtcode_core::core::agent::snapshots::{
        DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS,
    };
    use vtcode_core::core::threads::{ThreadBootstrap, ThreadManager};

    static PROMPT_PROVIDER_TEST_LOCK: AsyncMutex<()> = AsyncMutex::const_new(());

    struct PromptProviderFactoryGuard;

    impl PromptProviderFactoryGuard {
        fn install(provider_name: &str, factory: Arc<PromptProviderFactory>) -> Self {
            *PROMPT_PROVIDER_OVERRIDE.lock().expect("prompt provider factory lock") =
                Some(PromptProviderOverride { provider_name: provider_name.to_string(), factory });
            Self
        }
    }

    impl Drop for PromptProviderFactoryGuard {
        fn drop(&mut self) {
            *PROMPT_PROVIDER_OVERRIDE.lock().expect("prompt provider factory lock") = None;
        }
    }

    #[test]
    fn advertised_capabilities_include_session_discovery_and_resume() {
        let capabilities = advertised_agent_capabilities();

        assert!(capabilities.load_session);
        assert!(capabilities.session_capabilities.list.is_some());
        assert!(capabilities.session_capabilities.resume.is_some());
    }
    use vtcode_core::llm::provider::{LLMError, LLMErrorMetadata};

    use super::*;

    #[test]
    fn provider_error_telemetry_exposes_structured_network_diagnostics() {
        let error = LLMError::Network {
            message: "request failed".to_string(),
            metadata: Some(LLMErrorMetadata::new(
                "Arli AI",
                Some(504),
                Some("reqwest_timeout_error".to_string()),
                None,
                None,
                None,
                Some("operation timed out".to_string()),
            )),
        };

        assert_eq!(
            provider_error_telemetry(&error),
            ProviderErrorTelemetry {
                code: Some("reqwest_timeout_error"),
                status: Some(504),
                detail: Some("operation timed out"),
            }
        );
    }

    proptest! {
        #[test]
        fn streaming_eligibility_depends_only_on_provider_support_and_stop_hooks(
            supports_streaming in any::<bool>(),
            tools_allowed in any::<bool>(),
            has_stop_hooks in any::<bool>(),
        ) {
            prop_assert_eq!(
                should_allow_streaming(supports_streaming, tools_allowed, has_stop_hooks),
                supports_streaming && !has_stop_hooks,
            );
        }

        #[test]
        fn observing_stream_output_replaces_only_the_first_token_deadline(
            first_token_secs in prop::option::of(1u64..3_601),
            idle_secs in prop::option::of(1u64..3_601),
            total_secs in prop::option::of(1u64..7_201),
            observed_after_secs in 0u64..3_601,
        ) {
            let policy = ProviderDeadlinePolicy {
                connect: None,
                first_token: first_token_secs.map(Duration::from_secs),
                stream_idle: idle_secs.map(Duration::from_secs),
                total_generation: total_secs.map(Duration::from_secs),
            };
            let started_at = Instant::now();
            let observed_at = started_at + Duration::from_secs(observed_after_secs);
            let mut tracker = StreamDeadlineTracker::new(policy, started_at);
            let original_total = tracker.total;

            tracker.observe_output_at(observed_at);

            prop_assert_eq!(tracker.first_token, None);
            prop_assert_eq!(tracker.idle, deadline_from(observed_at, policy.stream_idle));
            prop_assert_eq!(tracker.total, original_total);
        }
    }

    #[test]
    fn failed_turn_releases_session_and_preserves_gathered_context() {
        let thread = ThreadManager::new().start_thread_with_identifier("acp-recovery", ThreadBootstrap::new(None));
        thread.append_message(Message::user("original request".to_string()));

        {
            let _guard = TurnGuard::begin(thread.clone()).expect("first turn should begin");
            let failed_turn = IncompleteProviderTurn::from_failure(
                "context gathered from a file read",
                "partial reasoning",
                "\u{1b}[31m502 Bad Gateway\u{1b}[0m",
            );
            assert_eq!(failed_turn.response.stop_reason, acp::StopReason::EndTurn);
            assert!(failed_turn.visible_update.starts_with("\n\n"));
            assert!(failed_turn.visible_update.contains("You can retry the prompt"));
            assert!(!failed_turn.visible_update.contains('\u{1b}'));
            thread.append_message(failed_turn.message);
        }

        let messages = thread.messages();
        assert_eq!(messages.len(), 2, "failed turn history must remain available to continue");
        assert!(messages[1].content.as_text().contains("context gathered from a file read"));
        assert!(messages[1].content.as_text().contains("You can retry the prompt"));
        assert_eq!(messages[1].reasoning.as_deref(), Some("partial reasoning"));
        let metadata = messages[1].metadata.as_ref().expect("incomplete response metadata");
        assert!(metadata.is_incomplete());
        assert_eq!(metadata.incomplete_reason(), Some("502 Bad Gateway"));
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

    #[tokio::test]
    async fn configured_non_stop_hooks_do_not_disable_streaming() {
        use assert_fs::TempDir;
        use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
        use vtcode_core::hooks::{LifecycleHookEngine, SessionStartTrigger};

        let workspace = TempDir::new().expect("temporary hook workspace");
        let config = HooksConfig {
            lifecycle: LifecycleHooksConfig {
                user_prompt_submit: vec![HookGroupConfig {
                    matcher: None,
                    hooks: vec![HookCommandConfig {
                        command: "printf ok".to_string(),
                        ..Default::default()
                    }],
                }],
                ..Default::default()
            },
        };
        let engine = LifecycleHookEngine::new_with_session_gated(
            workspace.path().to_path_buf(),
            &config,
            SessionStartTrigger::NewSession,
            "acp-non-stop-streaming",
            false,
        )
        .expect("non-stop hook engine")
        .expect("configured non-stop hook");

        assert!(!engine.has_stop_hooks());
        assert!(should_allow_streaming(true, false, engine.has_stop_hooks()));
        assert!(should_allow_streaming(true, true, engine.has_stop_hooks()));
        assert!(!should_allow_streaming(false, false, engine.has_stop_hooks()));
        assert!(!should_allow_streaming(true, true, true));
    }

    #[test]
    fn finalization_only_emits_buffered_responses() {
        assert!(!should_emit_buffered_final_chunk(true), "streamed deltas are already visible");
        assert!(should_emit_buffered_final_chunk(false), "buffered drafts need one final chunk");
    }

    #[tokio::test]
    async fn blocked_stop_draft_is_not_visible_until_hook_allows_it() {
        use assert_fs::TempDir;
        use vtcode_core::config::{HookCommandConfig, HookGroupConfig, HooksConfig, LifecycleHooksConfig};
        use vtcode_core::hooks::{LifecycleHookEngine, SessionStartTrigger};

        let workspace = TempDir::new().expect("temporary hook workspace");
        let config = HooksConfig {
            lifecycle: LifecycleHooksConfig {
                stop: vec![HookGroupConfig {
                    matcher: None,
                    hooks: vec![HookCommandConfig {
                        command: r#"count=$(cat "$VT_PROJECT_DIR/stop-count" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s' "$count" > "$VT_PROJECT_DIR/stop-count"; if [ "$count" -eq 1 ]; then printf '%s' '{"continue":false,"stopReason":"retry the draft"}'; fi"#
                            .to_string(),
                        ..HookCommandConfig::default()
                    }],
                }],
                ..LifecycleHooksConfig::default()
            },
        };
        let engine = LifecycleHookEngine::new_with_session_gated(
            workspace.path().to_path_buf(),
            &config,
            SessionStartTrigger::NewSession,
            "acp-stop-visibility",
            false,
        )
        .expect("stop hook engine")
        .expect("stop hook should be configured");

        assert!(engine.has_stop_hooks());
        let first = engine.run_stop("blocked draft", false).await.expect("first stop hook");
        let second = engine.run_stop("allowed draft", true).await.expect("second stop hook");

        let mut visible = Vec::new();
        let mut history = Vec::new();
        if let Some(reason) = first.block_reason {
            history.push(Message::assistant("blocked draft".to_string()));
            history.push(Message::system(reason));
        } else {
            visible.push("blocked draft".to_string());
        }
        if second.block_reason.is_none() {
            visible.push("allowed draft".to_string());
        }

        assert_eq!(visible, vec!["allowed draft"]);
        assert_eq!(history.len(), 2, "blocked drafts belong in model history only");
        assert_eq!(history[0].content.as_text(), "blocked draft");
        assert_eq!(history[1].content.as_text(), "retry the draft");
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

    #[test]
    fn partial_stream_message_preserves_output_and_marks_it_incomplete() {
        let failed_turn =
            IncompleteProviderTurn::from_failure("partial answer", "partial reasoning", "stream disconnected");
        let message = failed_turn.message;

        assert!(message.content.as_text().starts_with("partial answer"));
        assert!(message.content.as_text().contains("You can retry the prompt"));
        assert_eq!(message.reasoning.as_deref(), Some("partial reasoning"));
        let metadata = message.metadata.as_ref().expect("incomplete response metadata");
        assert!(metadata.is_incomplete());
        assert!(
            metadata
                .incomplete_reason()
                .is_some_and(|reason| reason.contains("stream disconnected"))
        );
    }

    #[test]
    fn provider_failure_without_output_becomes_a_normal_retryable_turn() {
        let failed_turn = IncompleteProviderTurn::from_failure("", "", "\u{1b}[31m502 Bad Gateway\u{1b}[0m");

        assert_eq!(failed_turn.response.stop_reason, acp::StopReason::EndTurn);
        assert_eq!(failed_turn.message.content.as_text(), failed_turn.visible_update);
        assert!(failed_turn.visible_update.contains("You can retry the prompt"));
        assert!(failed_turn.visible_update.contains("502 Bad Gateway"));
        assert!(!failed_turn.visible_update.contains('\u{1b}'));
        let metadata = failed_turn.message.metadata.as_ref().expect("incomplete response metadata");
        assert!(metadata.is_incomplete());
        assert_eq!(metadata.incomplete_reason(), Some("502 Bad Gateway"));
    }

    struct FailThenSucceedProvider {
        calls: Arc<AtomicUsize>,
    }

    struct StreamToolThenAnswerProvider {
        calls: Arc<AtomicUsize>,
        requests: Arc<Mutex<Vec<LLMRequest>>>,
        tool_calls: Vec<(String, String)>,
        mutation_before_call: Option<(usize, PathBuf, String)>,
    }

    struct PartialThenFailProvider;

    #[async_trait]
    impl LLMProvider for StreamToolThenAnswerProvider {
        fn name(&self) -> &str {
            "wire-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["wire-model".to_string()]
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn supports_tools(&self, _model: &str) -> bool {
            true
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn stream(&self, request: LLMRequest) -> Result<vtcode_core::llm::provider::LLMStream, LLMError> {
            self.requests.lock().expect("stream requests").push(request);
            let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some((index, path, content)) = self.mutation_before_call.as_ref()
                && *index == response_index
            {
                std::fs::write(path, content)
                    .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
            }
            let events = if let Some((tool_name, tool_arguments)) = self.tool_calls.get(response_index) {
                let response = LLMResponse {
                    content: Some("Checking files.".to_string()),
                    reasoning: Some("I need the workspace listing.".to_string()),
                    tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                        format!("call-tool-{response_index}"),
                        tool_name.clone(),
                        tool_arguments.clone(),
                    )]),
                    finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                    model: "wire-model".to_string(),
                    ..LLMResponse::default()
                };
                vec![
                    Ok(LLMStreamEvent::Reasoning { delta: "I need the workspace listing.".to_string() }),
                    Ok(LLMStreamEvent::Token { delta: "Checking files.".to_string() }),
                    Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
                ]
            } else {
                let response = LLMResponse {
                    content: Some("Tool complete.".to_string()),
                    finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                    model: "wire-model".to_string(),
                    ..LLMResponse::default()
                };
                vec![
                    Ok(LLMStreamEvent::Token { delta: "Tool complete.".to_string() }),
                    Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            Err(LLMError::InvalidRequest {
                message: "tool-enabled ACP turn used buffered generation".to_string(),
                metadata: None,
            })
        }
    }

    #[async_trait]
    impl LLMProvider for PartialThenFailProvider {
        fn name(&self) -> &str {
            "wire-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["wire-model".to_string()]
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn stream(&self, _request: LLMRequest) -> Result<vtcode_core::llm::provider::LLMStream, LLMError> {
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(LLMStreamEvent::Token { delta: "partial answer".to_string() }),
                Err(LLMError::Network {
                    message: "fixture stream disconnected".to_string(),
                    metadata: None,
                }),
            ])))
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            panic!("partial-stream test must not use buffered generation")
        }
    }

    #[async_trait]
    impl LLMProvider for FailThenSucceedProvider {
        fn name(&self) -> &str {
            "wire-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["wire-model".to_string()]
        }

        fn supports_streaming(&self) -> bool {
            false
        }

        fn supports_tools(&self, _model: &str) -> bool {
            false
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(LLMError::InvalidRequest {
                    message: "\u{1b}[31mfirst turn failed\u{1b}[0m".to_string(),
                    metadata: None,
                })
            } else {
                Ok(LLMResponse::new("wire-model", "second turn succeeded"))
            }
        }
    }

    async fn build_wire_test_agent(workspace: &std::path::Path) -> ZedAgent {
        let core_config = CoreAgentConfig {
            model: "wire-model".to_string(),
            api_key: "test-key".to_string(),
            provider: "wire-test".to_string(),
            api_key_env: "WIRE_TEST_API_KEY".to_string(),
            workspace: workspace.to_path_buf(),
            verbose: false,
            quiet: false,
            theme: "test".to_string(),
            reasoning_effort: ReasoningEffortLevel::Low,
            ui_surface: UiSurfacePreference::default(),
            prompt_cache: PromptCachingConfig::default(),
            model_source: ModelSelectionSource::WorkspaceConfig,
            custom_api_keys: BTreeMap::new(),
            checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
            checkpointing_storage_dir: None,
            checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
            checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
            max_conversation_turns: 1000,
            model_behavior: None,
            openai_chatgpt_auth: None,
        };
        let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
        discovery_input.include_user_agents = false;
        let discovered = vtcode_config::discover_subagents(&discovery_input).expect("discover primary agents");
        let primary_agents =
            crate::zed::helpers::PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

        Box::pin(ZedAgent::new(
            core_config,
            AuthCredentialsStoreMode::default(),
            AgentClientProtocolZedConfig::default(),
            ToolsConfig::default(),
            CommandsConfig::default(),
            &[],
            vtcode_config::TimeoutsConfig::default(),
            String::new(),
            Some("Wire test".to_string()),
            primary_agents,
            true,
            None,
            None,
        ))
        .await
    }

    #[tokio::test]
    async fn provider_failure_is_normal_end_turn_and_same_session_accepts_a_second_prompt() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let factory_calls = Arc::clone(&calls);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || Box::new(FailThenSucceedProvider { calls: Arc::clone(&factory_calls) })),
        );
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

        let agent_connection = install_handlers(Agent.builder().name("vtcode-wire-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                let _initialize = cx
                    .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?;
                let session = cx
                    .send_request(NewSessionRequest::new(workspace.path().to_path_buf()).meta(acp::Meta::from_iter([
                        ("client".to_string(), serde_json::json!("wire-test")),
                        ("requestId".to_string(), serde_json::json!("create-1")),
                    ])))
                    .block_task()
                    .await?;
                let first = cx
                    .send_request(
                        PromptRequest::new(
                            session.session_id.clone(),
                            vec![acp::ContentBlock::Text(acp::TextContent::new("first prompt"))],
                        )
                        .meta(acp::Meta::from_iter([("requestId".to_string(), serde_json::json!("prompt-1"))])),
                    )
                    .block_task()
                    .await?;
                assert_eq!(first.stop_reason, acp::StopReason::EndTurn);
                let second = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("second prompt"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(second.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("client protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(updates.iter().any(|notification| match &notification.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(text) => {
                    text.text.contains("You can retry the prompt") && !text.text.contains('\u{1b}')
                }
                _ => false,
            },
            _ => false,
        }));
        assert!(updates.iter().any(|notification| match &notification.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(text) => text.text == "second turn succeeded",
                _ => false,
            },
            _ => false,
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let session = agent
            .sessions
            .lock()
            .expect("ACP session map")
            .values()
            .next()
            .expect("wire ACP session")
            .clone();
        let (metadata, archive_path) = {
            let data = session.data.lock().expect("wire ACP session data");
            let metadata = data.thread.metadata().expect("wire ACP session metadata");
            let archive_path = data.archive.as_ref().expect("wire ACP session archive").path().to_path_buf();
            (metadata, archive_path)
        };
        assert_eq!(
            metadata.acp_meta.as_ref().and_then(|meta| meta.get("client")),
            Some(&serde_json::json!("wire-test"))
        );
        assert_eq!(
            metadata.acp_meta.as_ref().and_then(|meta| meta.get("requestId")),
            Some(&serde_json::json!("prompt-1"))
        );
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&archive_path).expect("read persisted wire ACP session archive"))
                .expect("parse persisted wire ACP session archive");
        assert_eq!(persisted["metadata"]["acp_meta"]["client"], "wire-test");
        assert_eq!(persisted["metadata"]["acp_meta"]["requestId"], "prompt-1");
        std::fs::remove_file(archive_path).expect("remove persisted wire ACP session archive");
    }

    #[tokio::test]
    async fn streamed_tool_call_runs_tool_loop_before_streamed_final_answer() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory_calls = Arc::clone(&calls);
        let factory_requests = Arc::clone(&requests);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(StreamToolThenAnswerProvider {
                    calls: Arc::clone(&factory_calls),
                    requests: Arc::clone(&factory_requests),
                    tool_calls: vec![("list_files".to_string(), r#"{"path":""}"#.to_string())],
                    mutation_before_call: None,
                })
            }),
        );
        let launch_workspace = TempDir::new().expect("wire test launch workspace");
        let workspace = TempDir::new().expect("wire test requested workspace");
        std::fs::write(workspace.path().join("visible.txt"), "fixture").expect("write workspace fixture");
        std::fs::write(launch_workspace.path().join("wrong-root.txt"), "fixture")
            .expect("write launch workspace fixture");
        let agent = Arc::new(build_wire_test_agent(launch_workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

        let agent_connection = install_handlers(Agent.builder().name("vtcode-stream-tool-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx
                    .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                    .block_task()
                    .await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Inspect the workspace"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("streamed tool protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
        assert!(
            updates
                .iter()
                .all(|notification| !matches!(&notification.update, acp::SessionUpdate::Plan(_))),
            "ACP must not publish synthetic progress entries unrelated to model-managed tasks"
        );
        let visible_text = updates
            .iter()
            .filter_map(|notification| match &notification.update {
                acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                    acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(visible_text.contains(&"Checking files."));
        assert!(visible_text.contains(&"Tool complete."));
        assert_eq!(calls.load(Ordering::SeqCst), 2, "one stream before and one stream after the tool");

        let requests = requests.lock().expect("stream requests");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.stream));
        assert!(
            requests[0]
                .tools
                .as_deref()
                .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "task_tracker")),
            "ACP provider requests must expose task_tracker to the model"
        );
        assert!(
            requests[1].messages.iter().any(Message::is_tool_response),
            "the second stream must include the executed tool result"
        );
        let tool_response = requests[1]
            .messages
            .iter()
            .find(|message| message.is_tool_response())
            .map(|message| message.content.as_text())
            .expect("list_files tool response");
        assert!(tool_response.contains("visible.txt"), "tool must use session/new cwd: {tool_response}");
        assert!(!tool_response.contains("wrong-root.txt"), "tool must not use the ACP launch cwd: {tool_response}");
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|message| message.reasoning.as_deref() == Some("I need the workspace listing.")),
            "the assistant tool-call checkpoint must retain streamed reasoning"
        );
    }

    #[tokio::test]
    async fn task_tracker_tool_result_emits_standard_acp_plan() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let task_arguments = serde_json::json!({
            "action": "create",
            "title": "Real model-managed work",
            "items": [
                {
                    "description": "Implement ACP plan rendering",
                    "status": "in_progress",
                    "files": ["src/progress.rs"],
                    "verify": ["cargo test"]
                },
                {
                    "description": "Publish the change",
                    "status": "blocked",
                    "outcome": "Waiting for credentials"
                }
            ]
        })
        .to_string();
        let factory_calls = Arc::clone(&calls);
        let factory_requests = Arc::clone(&requests);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(StreamToolThenAnswerProvider {
                    calls: Arc::clone(&factory_calls),
                    requests: Arc::clone(&factory_requests),
                    tool_calls: vec![("task_tracker".to_string(), task_arguments.clone())],
                    mutation_before_call: None,
                })
            }),
        );
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

        let agent_connection = install_handlers(Agent.builder().name("vtcode-task-plan-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx
                    .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                    .block_task()
                    .await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Track this work"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Continue this work"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("task tracker protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        assert_eq!(calls.load(Ordering::SeqCst), 3, "the tracker result and next prompt must reach the provider");
        let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
        let plans = updates
            .iter()
            .filter_map(|notification| match &notification.update {
                acp::SessionUpdate::Plan(plan) => Some(plan),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(plans.len(), 2, "the tracker result and following prompt must each publish the persisted ACP plan");
        let plan = plans[0];
        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].content, "Implement ACP plan rendering");
        assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::InProgress);
        assert_eq!(plan.entries[1].content, "Publish the change [blocked]");
        assert_eq!(plan.entries[1].status, acp::PlanEntryStatus::Pending);
        assert_eq!(
            plan.meta.as_ref().and_then(|meta| meta.get("vtcode")),
            Some(&serde_json::json!({
                "taskTracker": {
                    "title": "Real model-managed work",
                    "total": 2,
                    "completed": 0,
                    "in_progress": 1,
                    "pending": 0,
                    "blocked": 1,
                    "progress_percent": 0,
                    "notes": null
                }
            }))
        );
        assert_eq!(plans[1], plan, "the next prompt must replay the same persisted plan");
    }

    #[tokio::test]
    async fn approved_apply_patch_executes_and_emits_preview_and_diff_over_acp() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let patch = "*** Begin Patch\n*** Add File: patched.txt\n+created over ACP\n*** End Patch\n";
        let patch_arguments = serde_json::json!({ "patch": patch }).to_string();
        let factory_calls = Arc::clone(&calls);
        let factory_requests = Arc::clone(&requests);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(StreamToolThenAnswerProvider {
                    calls: Arc::clone(&factory_calls),
                    requests: Arc::clone(&factory_requests),
                    tool_calls: vec![("apply_patch".to_string(), patch_arguments.clone())],
                    mutation_before_call: None,
                })
            }),
        );
        let workspace = TempDir::new().expect("wire test workspace");
        let workspace_path = workspace.path().to_path_buf();
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

        let agent_connection = install_handlers(Agent.builder().name("vtcode-apply-patch-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Create patched.txt"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("apply_patch protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        assert_eq!(
            std::fs::read_to_string(workspace.path().join("patched.txt")).expect("read patched file"),
            "created over ACP\n"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");

        let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
        let preview_visible = updates.iter().any(|notification| {
            let acp::SessionUpdate::ToolCall(call) = &notification.update else {
                return false;
            };
            call.content.iter().any(|content| {
                matches!(
                    content,
                    acp::ToolCallContent::Content(block)
                        if matches!(&block.content, acp::ContentBlock::Text(text) if text.text == patch)
                )
            })
        });
        assert!(preview_visible, "the initial ACP tool call must expose the patch text");

        let diff = updates.iter().find_map(|notification| {
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return None;
            };
            update.fields.content.as_ref()?.iter().find_map(|content| match content {
                acp::ToolCallContent::Diff(diff) => Some(diff),
                _ => None,
            })
        });
        let diff = diff.expect("the completed ACP tool update must include a standardized diff");
        assert_eq!(diff.path, workspace.path().join("patched.txt"));
        assert_eq!(diff.old_text, None);
        assert_eq!(diff.new_text, "created over ACP\n");
    }

    #[tokio::test]
    async fn file_versions_and_no_op_guard_survive_the_official_acp_transport() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let workspace = TempDir::new().expect("wire test workspace");
        let versioned_path = workspace.path().join("versioned.txt");
        std::fs::write(&versioned_path, "before\n").expect("write versioned fixture");
        let before_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"before\n"));
        let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"));
        let read = serde_json::json!({"path": "versioned.txt", "line": 1, "limit": 1}).to_string();
        let stale = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-before\n+done\n*** End Patch\n";
        let no_op = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+current\n*** End Patch\n";
        let different = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+done\n*** End Patch\n";
        let tool_calls = vec![
            ("read_file".to_string(), read.clone()),
            (
                "apply_patch".to_string(),
                serde_json::json!({"input": stale, "expected_content_hash": before_hash.clone()}).to_string(),
            ),
            ("read_file".to_string(), read),
            (
                "apply_patch".to_string(),
                serde_json::json!({"input": no_op, "expected_content_hash": current_hash.clone()}).to_string(),
            ),
            ("apply_patch".to_string(), serde_json::json!({"patch": no_op}).to_string()),
            ("apply_patch".to_string(), serde_json::json!({"input": no_op}).to_string()),
            ("apply_patch".to_string(), serde_json::json!({"input": different}).to_string()),
        ];
        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory_calls = Arc::clone(&calls);
        let factory_requests = Arc::clone(&requests);
        let mutation_path = versioned_path.clone();
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(StreamToolThenAnswerProvider {
                    calls: Arc::clone(&factory_calls),
                    requests: Arc::clone(&factory_requests),
                    tool_calls: tool_calls.clone(),
                    mutation_before_call: Some((1, mutation_path.clone(), "current\n".to_string())),
                })
            }),
        );
        let workspace_path = workspace.path().to_path_buf();
        let client_workspace_path = workspace_path.clone();
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();

        let agent_connection = install_handlers(
            Agent.builder().name("vtcode-read-file-version-test"),
            Arc::clone(&agent),
        )
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_request(
                async move |request: acp::ReadTextFileRequest, responder, _connection| {
                    assert_eq!(request.path, client_workspace_path.join("versioned.txt"));
                    let content = std::fs::read_to_string(&request.path).expect("read client fixture");
                    responder.respond(acp::ReadTextFileResponse::new(content))
                },
                on_receive_request!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Version and patch versioned.txt",
                        ))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("versioned patch protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        assert_eq!(calls.load(Ordering::SeqCst), 8, "all seven tool results must return to the provider");
        let requests = requests.lock().expect("stream requests");
        let payload = |index: usize| {
            let message = requests[index]
                .messages
                .iter()
                .rev()
                .find(|message| message.tool_call_id.is_some())
                .expect("tool result message");
            serde_json::from_str::<serde_json::Value>(&message.content.as_text()).expect("structured tool result")
        };
        assert_eq!(payload(1)["content_hash"], before_hash);
        assert_eq!(payload(2)["error"]["details"]["reason"], "content_hash_mismatch");
        assert_eq!(payload(3)["content_hash"], current_hash);
        assert_eq!(payload(4)["result"]["occurrence"], 1);
        assert_eq!(payload(5)["result"]["retry_prohibited"], true);
        assert_eq!(payload(6)["error"]["details"]["reason"], "repeated_identical_no_op");
        assert_eq!(payload(7)["result"]["success"], true);
        assert_eq!(std::fs::read_to_string(versioned_path).expect("read final fixture"), "done\n");
    }

    #[tokio::test]
    async fn visible_stream_failure_checkpoints_an_incomplete_turn_over_acp() {
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let _factory_guard =
            PromptProviderFactoryGuard::install("wire-test", Arc::new(|| Box::new(PartialThenFailProvider)));
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

        let agent_connection = install_handlers(Agent.builder().name("vtcode-partial-stream-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx
                    .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                    .block_task()
                    .await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Begin a response"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("partial-stream protocol flow should end normally");
        agent_task.abort();
        drop(agent_task.await);

        let visible_text = std::iter::from_fn(|| updates_rx.try_recv().ok())
            .filter_map(|notification| match notification.update {
                acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
                    acp::ContentBlock::Text(text) => Some(text.text),
                    _ => None,
                },
                _ => None,
            })
            .collect::<String>();
        assert!(visible_text.contains("partial answer"));
        assert!(visible_text.contains("You can retry the prompt"));

        let session = agent
            .sessions
            .lock()
            .expect("ACP session map")
            .values()
            .next()
            .expect("wire session")
            .clone();
        let messages = session.data.lock().expect("wire session data").thread.messages();
        let incomplete = messages.last().expect("incomplete assistant message");
        assert!(incomplete.content.as_text().contains("partial answer"));
        assert!(incomplete.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete));
    }

    #[tokio::test]
    async fn acp_local_exec_preserves_multiline_public_command_arguments() {
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = build_wire_test_agent(workspace.path()).await;
        let multiline = "printf '%s' 'ACP first line\nACP second line\n'";

        for args in [
            serde_json::json!({"cmd": multiline}),
            serde_json::json!({"cmd": multiline, "justification": "exercise ACP argument routing"}),
        ] {
            let report = agent
                .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-multiline")
                .await;

            assert_eq!(
                report.status,
                acp::ToolCallStatus::Completed,
                "multiline public command failed: {}",
                report.llm_response
            );
            let payload: serde_json::Value =
                serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
            let output = payload["result"]
                .get("output")
                .or_else(|| payload["result"].get("stdout"))
                .and_then(serde_json::Value::as_str)
                .expect("successful command response must contain text output");
            assert_eq!(output, "ACP first line\nACP second line\n");
            assert!(!report.llm_response.contains("Missing required argument: command"));
        }
    }

    #[tokio::test]
    async fn acp_local_exec_rejects_missing_public_command_argument() {
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = build_wire_test_agent(workspace.path()).await;

        let report = agent
            .execute_local_tool(
                vtcode_core::config::constants::tools::EXEC_COMMAND,
                &serde_json::json!({}),
                "call-empty",
            )
            .await;

        assert_eq!(report.status, acp::ToolCallStatus::Failed);
        assert!(report.llm_response.contains("Missing required argument: command"));
    }

    struct FlakyProvider {
        attempts: AtomicUsize,
        network_failures: usize,
        invalid_request: bool,
    }

    struct PendingProvider;

    struct BadGatewayProvider {
        attempts: AtomicUsize,
    }

    #[async_trait]
    impl LLMProvider for PendingProvider {
        fn name(&self) -> &str {
            "timeout-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["test-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            std::future::pending().await
        }
    }

    #[async_trait]
    impl LLMProvider for BadGatewayProvider {
        fn name(&self) -> &str {
            "bad-gateway-test"
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["test-model".to_string()]
        }

        fn supports_streaming(&self) -> bool {
            false
        }

        fn supports_tools(&self, _model: &str) -> bool {
            false
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            let _previous_attempts = self.attempts.fetch_add(1, Ordering::SeqCst);
            Err(LLMError::Provider {
                message: "OpenAI Chat Completions error (status 502 Bad Gateway)".to_string(),
                metadata: Some(LLMErrorMetadata::new(
                    "openai",
                    Some(502),
                    None,
                    None,
                    None,
                    None,
                    Some("502 Bad Gateway".to_string()),
                )),
            })
        }
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
        super::super::super::provider_runtime::ProviderRuntimeRegistry::new(
            &[provider],
            &vtcode_config::TimeoutsConfig::default(),
        )
        .for_provider("retry-test")
    }

    fn timeout_runtime() -> ProviderRequestRuntime {
        let provider = CustomProviderConfig {
            name: "timeout-test".to_string(),
            request_policy: CustomProviderRequestPolicyConfig {
                max_retries: 0,
                total_generation_timeout_seconds: 1,
                ..CustomProviderRequestPolicyConfig::default()
            },
            ..CustomProviderConfig::default()
        };
        super::super::super::provider_runtime::ProviderRuntimeRegistry::new(
            &[provider],
            &vtcode_config::TimeoutsConfig::default(),
        )
        .for_provider("timeout-test")
    }

    #[tokio::test]
    async fn buffered_generation_obeys_total_generation_timeout() {
        let result = generate_with_retry(
            &PendingProvider,
            LLMRequest::default(),
            &timeout_runtime(),
            &super::super::super::types::SessionCancellation::default(),
        )
        .await;

        let Err(ProviderCallError::Failed(error)) = result else {
            panic!("pending generation should fail at its total deadline");
        };
        assert!(error.contains("total generation"));
    }

    #[test]
    fn stream_deadline_moves_from_first_token_to_idle_without_resetting_total() {
        let policy = ProviderDeadlinePolicy {
            connect: Some(Duration::from_secs(30)),
            first_token: Some(Duration::from_secs(180)),
            stream_idle: Some(Duration::from_secs(120)),
            total_generation: Some(Duration::from_secs(600)),
        };
        let started_at = Instant::now();
        let total = deadline_from(started_at, policy.total_generation);
        let mut tracker = StreamDeadlineTracker::new(policy, started_at);

        assert_eq!(tracker.next().map(|(phase, _deadline)| phase), Some(StreamTimeoutPhase::FirstToken));
        let first_output_at = started_at + Duration::from_secs(20);
        tracker.observe_output_at(first_output_at);
        assert_eq!(tracker.next().map(|(phase, _deadline)| phase), Some(StreamTimeoutPhase::InterTokenIdle));
        assert_eq!(tracker.idle, deadline_from(first_output_at, policy.stream_idle));
        assert_eq!(tracker.total, total, "observing output must not extend the total generation deadline");
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
    async fn bad_gateway_retries_until_the_configured_budget_is_exhausted() {
        let provider = BadGatewayProvider { attempts: AtomicUsize::new(0) };

        let result = generate_with_retry(
            &provider,
            LLMRequest::default(),
            &retry_runtime(),
            &super::super::super::types::SessionCancellation::default(),
        )
        .await;

        let Err(ProviderCallError::Failed(error)) = result else {
            panic!("persistent 502 response should exhaust the retry budget");
        };
        assert!(error.contains("502 Bad Gateway"));
        assert_eq!(provider.attempts.load(Ordering::SeqCst), 3);
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
