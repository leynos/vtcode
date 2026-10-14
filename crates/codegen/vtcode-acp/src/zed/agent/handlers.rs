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
use super::compaction::CompactionRequest;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::tooling::SupportedTool;
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
use std::fmt::Display;
use std::sync::Arc;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tracing::{Instrument, Span, debug, info, warn};
use vtcode_commons::ansi::strip_ansi_codes;
use vtcode_core::config::api_keys::{ApiKeySources, get_api_key_with_mode};
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::factory::ProviderConfig;
use vtcode_core::llm::factory::create_provider_with_config;
use vtcode_core::llm::provider::{
    LLMError, LLMErrorMetadata, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent, Message,
    ToolCall as LLMToolCall, ToolDefinition,
};
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

fn record_stream_attempt(span: &Span, started_at: Instant, outcome: &'static str) {
    let _ = span.record("outcome", outcome);
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _ = span.record("elapsed_ms", elapsed_ms);
}

fn poll_provider_stream<S>(stream: &mut S, span: Span) -> impl Future<Output = Option<S::Item>> + '_
where
    S: futures::Stream + Unpin,
{
    stream.next().instrument(span)
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

fn provider_timeout_error(provider: &str, phase: StreamTimeoutPhase, timeout: Option<Duration>) -> LLMError {
    let duration = timeout.map_or_else(|| "configured deadline".to_string(), |timeout| format!("{timeout:?}"));
    LLMError::Network {
        message: format!("provider '{provider}' exceeded its {} timeout ({duration})", phase.label()),
        metadata: Some(LLMErrorMetadata::new(
            "ACP",
            None,
            Some(phase.error_code().to_string()),
            None,
            None,
            None,
            None,
        )),
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

#[tracing::instrument(skip_all, fields(boundary = boundary))]
async fn persist_session_checkpoint(agent: &ZedAgent, session: &SessionHandle, boundary: &'static str) {
    if let Err(error) = agent.checkpoint_session(session).await {
        warn!(%error, boundary, "Failed to persist ACP session checkpoint");
    }
    if let Err(error) = session.update_transcript_path().await {
        warn!(%error, boundary, "Failed to update ACP session transcript path");
    }
}

async fn finish_failed_provider_turn(turn: &PromptExecution, error: &str) -> Result<PromptResponse, SdkError> {
    let IncompleteProviderTurn { message, visible_update, response } =
        IncompleteProviderTurn::from_failure(&turn.assistant_message, &turn.assistant_reasoning, error);
    drop(
        turn.agent
            .send_update(&turn.args.session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(visible_update)))
            .await,
    );
    push_session_message(&turn.agent, &turn.session, &turn.args.session_id, message)?;
    persist_session_checkpoint(&turn.agent, &turn.session, "incomplete_provider_turn").await;
    warn!(
        partial_text_bytes = turn.assistant_message.len(),
        partial_reasoning_bytes = turn.assistant_reasoning.len(),
        "ACP provider failed to complete the turn"
    );
    Ok(response)
}

async fn finish_failed_output(
    turn: &mut PromptExecution,
    output: ProviderOutput,
    error: &str,
) -> Result<ProviderTurnOutcome, SdkError> {
    output.store_in(turn);
    finish_failed_provider_turn(turn, error)
        .await
        .map(ProviderTurnOutcome::Response)
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

    const fn error_code(self) -> &'static str {
        match self {
            Self::FirstToken => "acp_timeout_first_token",
            Self::InterTokenIdle => "acp_timeout_inter_token_idle",
            Self::TotalGeneration => "acp_timeout_total_generation",
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
    // Starts before calling the provider router, so first-token and total
    // latency include any wait at the router's shared admission gate.
    started_at: Instant,
    first_output_at: Option<Instant>,
    estimated_output_tokens: u64,
    runtime: ProviderRequestRuntime,
    finished: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct ProviderErrorTelemetry<'a> {
    code: Option<&'a str>,
    status: Option<u16>,
    timeout_phase: Option<&'static str>,
}

fn provider_error_telemetry(error: &LLMError) -> ProviderErrorTelemetry<'_> {
    let LLMError::Network { metadata: Some(metadata), .. } = error else {
        return ProviderErrorTelemetry::default();
    };
    let code = metadata.code.as_deref();
    let timeout_phase = match code {
        Some("acp_timeout_first_token") => Some("first_token"),
        Some("acp_timeout_inter_token_idle") => Some("inter_token_idle"),
        Some("acp_timeout_total_generation") => Some("total_generation"),
        _ => None,
    };
    ProviderErrorTelemetry {
        code: if timeout_phase.is_some() { None } else { code },
        status: metadata.status,
        timeout_phase,
    }
}
impl GenerationTelemetry {
    fn start(runtime: &ProviderRequestRuntime) -> Self {
        Self {
            started_at: Instant::now(),
            first_output_at: None,
            estimated_output_tokens: 0,
            runtime: runtime.clone(),
            finished: false,
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
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider produced its first output"
        );
    }

    fn complete(&mut self, runtime: &ProviderRequestRuntime, response: &LLMResponse, retry_count: u32, buffered: bool) {
        let elapsed = self.started_at.elapsed();
        let elapsed_ms = duration_millis(elapsed).max(1);
        let (output_tokens, token_count_source) = response
            .usage
            .as_ref()
            .filter(|usage| usage.completion_tokens > 0)
            .map(|usage| (u64::from(usage.completion_tokens), "provider"))
            .unwrap_or((self.estimated_output_tokens, "estimated"));
        let tokens_per_second = output_tokens.saturating_mul(1_000) / elapsed_ms;
        runtime.record_attempt_succeeded(
            elapsed_ms,
            self.first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at))),
        );
        self.finished = true;
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
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            "ACP provider generation completed"
        );
    }

    fn failed(
        &mut self,
        runtime: &ProviderRequestRuntime,
        retry_count: u32,
        retry_disposition: &'static str,
        error: &LLMError,
    ) {
        let elapsed_ms = duration_millis(self.started_at.elapsed());
        let first_output_ms = self
            .first_output_at
            .map(|first| duration_millis(first.duration_since(self.started_at)));
        runtime.record_attempt_failed(elapsed_ms, first_output_ms);
        if retry_disposition == "retry_scheduled" {
            runtime.record_retry_scheduled();
        }
        self.finished = true;
        let snapshot = runtime.telemetry_snapshot();
        let error_telemetry = provider_error_telemetry(error);
        warn!(
            provider = runtime.provider_name(),
            generation_elapsed_ms = duration_millis(self.started_at.elapsed()),
            retry_count,
            max_retries = runtime.retry_policy().max_attempts.saturating_sub(1),
            retry_disposition,
            acp_local_queue_depth = snapshot.acp_local_queue_depth,
            active_acp_requests = snapshot.active_acp_requests,
            configured_provider_limit = ?snapshot.configured_provider_limit,
            circuit_breaker_state = snapshot.circuit_breaker_state,
            provider_error_code = ?error_telemetry.code,
            provider_error_status = ?error_telemetry.status,
            provider_timeout_phase = error_telemetry.timeout_phase,
            "ACP provider generation attempt failed"
        );
    }
}

impl Drop for GenerationTelemetry {
    fn drop(&mut self) {
        if !self.finished {
            let first_output_ms = self
                .first_output_at
                .map(|first| duration_millis(first.duration_since(self.started_at)));
            self.runtime
                .record_attempt_cancelled(duration_millis(self.started_at.elapsed()), first_output_ms);
            self.finished = true;
        }
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
        let mut telemetry = GenerationTelemetry::start(runtime);
        let deadline_policy = runtime.deadline_policy();
        let total_deadline = deadline_after(deadline_policy.total_generation);
        let result = tokio::select! {
            () = cancellation.cancelled() => return Err(ProviderCallError::Cancelled),
            () = sleep_until_optional(total_deadline) => {
                Err(provider_timeout_error(
                    runtime.provider_name(),
                    StreamTimeoutPhase::TotalGeneration,
                    deadline_policy.total_generation,
                ))
            }
            result = provider.generate(request.clone()).instrument(tracing::info_span!(
                "acp_provider_generate",
                provider = runtime.provider_name()
            )) => result,
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
    *agent.client_capabilities.lock().map_err(|error| {
        SdkError::internal_error().data(format!("Failed to store ACP client capabilities: {error}"))
    })? = Some(caps);
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
    match agent.session_handle(&notif.session_id) {
        Ok(Some(session)) => session.cancellation.cancel(),
        Ok(None) => {}
        Err(error) => warn!(%error, session_id = %notif.session_id, "Failed to look up ACP session for cancellation"),
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
            let result = Box::pin(run_prompt(agent, req)).await;
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

struct PromptToolState {
    enabled_tools: Vec<SupportedTool>,
    primary_agent: String,
    has_local_tools: bool,
    tools_allowed: bool,
    tool_definitions: Option<Arc<Vec<ToolDefinition>>>,
    messages: Vec<Message>,
}

impl PromptToolState {
    fn new(enabled_tools: Vec<SupportedTool>, primary_agent: String, provider_supports_tools: bool) -> Self {
        Self {
            enabled_tools,
            primary_agent,
            has_local_tools: false,
            tools_allowed: provider_supports_tools,
            tool_definitions: None,
            messages: Vec::new(),
        }
    }

    fn refresh_availability(
        &mut self,
        agent: &ZedAgent,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        provider_supports_tools: bool,
    ) -> Result<(), SdkError> {
        self.primary_agent = session
            .data
            .lock()
            .map_err(|_err| SdkError::internal_error())?
            .primary_agent
            .clone();
        self.has_local_tools = agent
            .session_local_tools_available(session, &self.primary_agent)
            .map_err(|error| session_access_error(session_id, "checking local tool availability", error))?;
        self.tools_allowed = provider_supports_tools && (!self.enabled_tools.is_empty() || self.has_local_tools);
        self.tool_definitions = agent
            .session_tool_definitions(session, provider_supports_tools, &self.enabled_tools, &self.primary_agent)
            .map_err(|error| session_access_error(session_id, "resolving tool definitions", error))?
            .map(Arc::new);
        Ok(())
    }
}

struct ToolRoundRequest<'a> {
    agent: &'a ZedAgent,
    session: &'a SessionHandle,
    session_id: &'a acp::SessionId,
    tool_calls: &'a [LLMToolCall],
    assistant_message: &'a str,
    assistant_reasoning: &'a str,
    reasoning_details: Option<Vec<String>>,
    tool_loop_count: &'a mut usize,
    streamed: bool,
}

enum ToolRoundOutcome {
    Continue,
    Cancelled,
    LimitReached(String),
}

struct PromptInput {
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    session: SessionHandle,
    _turn_guard: TurnGuard,
    user_message: String,
}

enum PromptInputOutcome {
    Response(PromptResponse),
    Ready(PromptInput),
}

enum ProviderTurnOutcome {
    StopReason(acp::StopReason),
    Response(PromptResponse),
}

struct PromptExecution {
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    session: SessionHandle,
    _turn_guard: TurnGuard,
    provider: Box<dyn LLMProvider>,
    provider_runtime: ProviderRequestRuntime,
    session_model: String,
    reasoning_effort: Option<ReasoningEffortLevel>,
    provider_supports_tools: bool,
    allow_streaming: bool,
    tool_state: PromptToolState,
    assistant_message: String,
    assistant_reasoning: String,
    stop_hook_active: bool,
    stop_hook_checked: bool,
}

async fn execute_tool_round(request: ToolRoundRequest<'_>) -> Result<ToolRoundOutcome, SdkError> {
    let ToolRoundRequest {
        agent,
        session,
        session_id,
        tool_calls,
        assistant_message,
        assistant_reasoning,
        reasoning_details,
        tool_loop_count,
        streamed,
    } = request;

    if agent.tool_loop_limit_reached(*tool_loop_count) {
        return Ok(ToolRoundOutcome::LimitReached(agent.tool_loop_limit_message()));
    }
    *tool_loop_count = tool_loop_count.saturating_add(1);

    let assistant_tool_message = if streamed {
        assistant_tool_call_message(
            assistant_message.to_string(),
            tool_calls.to_vec(),
            assistant_reasoning,
            reasoning_details,
        )
    } else {
        Message::assistant_with_tools(assistant_message.to_string(), tool_calls.to_vec())
    };
    push_session_message(agent, session, session_id, assistant_tool_message)?;
    persist_session_checkpoint(agent, session, "assistant_tool_calls").await;

    let tool_results = match agent.execute_tool_calls(session, session_id, tool_calls).await {
        Ok(results) => results,
        Err(error) => {
            warn!(%error, "Tool execution failed");
            for call in tool_calls {
                push_session_message(
                    agent,
                    session,
                    session_id,
                    Message::tool_response(call.id.clone(), format!("Tool execution was interrupted: {error}")),
                )?;
            }
            persist_session_checkpoint(agent, session, "interrupted_tool_results").await;
            return Err(error);
        }
    };

    for result in tool_results {
        push_session_message(
            agent,
            session,
            session_id,
            Message::tool_response(result.tool_call_id, result.llm_response),
        )?;
    }
    persist_session_checkpoint(agent, session, "tool_results").await;

    Ok(if session.cancellation.is_cancelled() {
        ToolRoundOutcome::Cancelled
    } else {
        ToolRoundOutcome::Continue
    })
}

async fn prepare_prompt_input(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptInputOutcome, SdkError> {
    let Some(session) = agent
        .session_handle(&args.session_id)
        .map_err(|error| session_access_error(&args.session_id, "looking up session", error))?
    else {
        return Err(SdkError::invalid_params().data(json!({ "reason": "unknown_session" })));
    };

    if let Err(error) = agent.replay_persisted_task_plan(&session, &args.session_id).await {
        warn!(%error, session_id = %args.session_id, "Failed to replay persisted ACP task plan");
    }

    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    let turn_guard = TurnGuard::begin(thread)?;
    start_prompt_safety_budget(&agent, &session, &args.session_id)?;
    session.cancellation.reset();

    let user_message = tokio::select! {
        () = session.cancellation.cancelled() => {
            return Ok(PromptInputOutcome::Response(PromptResponse::new(acp::StopReason::Cancelled)));
        }
        result = agent.resolve_prompt(&session, &args.session_id, &args.prompt) => result?,
    };

    Ok(PromptInputOutcome::Ready(PromptInput {
        agent,
        args,
        session,
        _turn_guard: turn_guard,
        user_message,
    }))
}

fn start_prompt_safety_budget(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
) -> Result<(), SdkError> {
    if let Some(runtime) = session
        .workspace_runtime()
        .map_err(|error| session_access_error(session_id, "reading workspace runtime", error))?
    {
        runtime.local_tool_registry.safety_gateway().start_turn();
    } else {
        agent.local_tool_registry.safety_gateway().start_turn();
    }
    Ok(())
}

async fn accept_user_prompt(input: &PromptInput) -> Result<bool, SdkError> {
    let PromptInput { agent, args, session, user_message, .. } = input;
    if let Some(hooks) = session
        .lifecycle_hooks()
        .map_err(|error| session_access_error(&args.session_id, "reading lifecycle hooks", error))?
    {
        let outcome = hooks
            .run_user_prompt_submit(&args.session_id.to_string(), user_message)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            warn!(level = ?message.level, message = %message.text, "ACP UserPromptSubmit hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                push_session_message(agent, session, &args.session_id, Message::system(context))?;
            }
        }
        if !outcome.allow_prompt {
            let reason = outcome
                .block_reason
                .unwrap_or_else(|| "Prompt blocked by lifecycle hook".to_string());
            warn!(%reason, "ACP UserPromptSubmit hook blocked prompt");
            return Ok(false);
        }
    }

    push_session_message(agent, session, &args.session_id, Message::user(user_message.clone()))?;
    agent
        .merge_session_acp_meta(session, args.meta.clone())
        .map_err(|error| session_access_error(&args.session_id, "merging client metadata", error))?;
    persist_session_checkpoint(agent, session, "user_message").await;
    Ok(true)
}

async fn initialize_prompt_execution(input: PromptInput) -> Result<PromptExecution, SdkError> {
    let PromptInput { agent, args, session, _turn_guard, user_message: _ } = input;
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
            model_behaviour: agent.config.model_behaviour.clone(),
            workspace_root: Some(
                session
                    .workspace_runtime()
                    .map_err(|error| {
                        session_access_error(
                            &args.session_id,
                            "reading workspace runtime for provider configuration",
                            error,
                        )
                    })?
                    .map_or_else(|| agent.config.workspace.clone(), |runtime| runtime.workspace_root.clone()),
            ),
        },
    )
    .map_err(|err| SdkError::internal_error().data(err.to_string()))?;

    let supports_streaming = provider.supports_streaming();
    let reasoning_effort = provider
        .supports_reasoning_effort(&session_model)
        .then_some(session_reasoning_effort);
    let client_supports_read_text_file = agent
        .client_supports_read_text_file()
        .map_err(|error| session_access_error(&args.session_id, "reading client filesystem capabilities", error))?;
    let provider_supports_tools = provider.supports_tools(&session_model);
    let primary_agent = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        data.primary_agent.clone()
    };
    let enabled_tools = agent
        .tool_availability(provider_supports_tools, client_supports_read_text_file)
        .into_iter()
        .filter_map(|(tool, runtime)| matches!(runtime, ToolRuntime::Enabled).then_some(tool))
        .collect();
    let mut tool_state = PromptToolState::new(enabled_tools, primary_agent, provider_supports_tools);
    tool_state.refresh_availability(&agent, &session, &args.session_id, provider_supports_tools)?;
    tool_state.messages = agent
        .resolved_messages(&session)
        .map_err(|error| session_access_error(&args.session_id, "resolving session messages", error))?;
    if maybe_compact_prompt(&agent, &session, provider.as_ref(), &provider_runtime, &session_model, &tool_state).await?
    {
        tool_state.messages = agent
            .resolved_messages(&session)
            .map_err(|error| session_access_error(&args.session_id, "resolving compacted session messages", error))?;
    }

    let has_stop_hooks = session
        .has_stop_hooks()
        .map_err(|error| session_access_error(&args.session_id, "checking Stop hooks", error))?;
    let allow_streaming = should_allow_streaming(supports_streaming, tool_state.tools_allowed, has_stop_hooks);

    Ok(PromptExecution {
        agent,
        args,
        session,
        _turn_guard,
        provider,
        provider_runtime,
        session_model,
        reasoning_effort,
        provider_supports_tools,
        allow_streaming,
        tool_state,
        assistant_message: String::with_capacity(4096),
        assistant_reasoning: String::with_capacity(2048),
        stop_hook_active: false,
        stop_hook_checked: false,
    })
}

async fn maybe_compact_prompt(
    agent: &ZedAgent,
    session: &SessionHandle,
    provider: &dyn LLMProvider,
    provider_runtime: &ProviderRequestRuntime,
    session_model: &str,
    tool_state: &PromptToolState,
) -> Result<bool, SdkError> {
    agent
        .maybe_compact_session(CompactionRequest {
            session,
            provider,
            runtime: provider_runtime,
            model: session_model,
            tools: tool_state.tool_definitions.as_ref(),
        })
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))
}

async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
    let input = match prepare_prompt_input(agent, args).await? {
        PromptInputOutcome::Response(response) => return Ok(response),
        PromptInputOutcome::Ready(input) => input,
    };
    if !accept_user_prompt(&input).await? {
        return Ok(PromptResponse::new(acp::StopReason::Refusal));
    }
    initialize_prompt_execution(input).await?.run().await
}

impl PromptExecution {
    async fn run(mut self) -> Result<PromptResponse, SdkError> {
        let outcome = if self.allow_streaming {
            run_streaming_provider_turn(&mut self).await?
        } else {
            run_buffered_provider_turn(&mut self).await?
        };
        match outcome {
            ProviderTurnOutcome::StopReason(stop_reason) => self.finish(stop_reason).await,
            ProviderTurnOutcome::Response(response) => Ok(response),
        }
    }

    async fn finish(self, stop_reason: acp::StopReason) -> Result<PromptResponse, SdkError> {
        if stop_reason != acp::StopReason::Cancelled {
            if !self.stop_hook_checked
                && let Some(reason) = self
                    .agent
                    .run_stop_hook(&self.session, &self.assistant_message, self.stop_hook_active)
                    .await?
            {
                warn!(%reason, "ACP Stop hook blocked final turn");
                push_session_message(&self.agent, &self.session, &self.args.session_id, Message::system(reason))?;
                persist_session_checkpoint(&self.agent, &self.session, "stop_hook_blocked").await;
                return Ok(PromptResponse::new(acp::StopReason::Refusal));
            }
            if !self.assistant_message.is_empty() {
                if should_emit_buffered_final_chunk(self.allow_streaming) {
                    drop(
                        self.agent
                            .send_update(
                                &self.args.session_id,
                                acp::SessionUpdate::AgentMessageChunk(text_chunk(self.assistant_message.clone())),
                            )
                            .await,
                    );
                }
                let mut completed_message = Message::assistant(self.assistant_message);
                if !self.assistant_reasoning.is_empty() {
                    completed_message.reasoning = Some(self.assistant_reasoning);
                }
                push_session_message(&self.agent, &self.session, &self.args.session_id, completed_message)?;
                persist_session_checkpoint(&self.agent, &self.session, "assistant_response").await;
            }
        }
        Ok(PromptResponse::new(stop_reason))
    }
}
struct ProviderOutput {
    assistant_message: String,
    assistant_reasoning: String,
    emitted_output: bool,
}

impl ProviderOutput {
    fn empty() -> Self {
        Self {
            assistant_message: String::new(),
            assistant_reasoning: String::new(),
            emitted_output: false,
        }
    }

    fn take_from(turn: &mut PromptExecution) -> Self {
        let assistant_message = std::mem::take(&mut turn.assistant_message);
        let assistant_reasoning = std::mem::take(&mut turn.assistant_reasoning);
        let emitted_output = !assistant_message.is_empty() || !assistant_reasoning.is_empty();
        Self {
            assistant_message,
            assistant_reasoning,
            emitted_output,
        }
    }

    fn reset(&mut self) {
        self.assistant_message.clear();
        self.assistant_reasoning.clear();
        self.emitted_output = false;
    }

    fn store_in(self, turn: &mut PromptExecution) {
        turn.assistant_message = self.assistant_message;
        turn.assistant_reasoning = self.assistant_reasoning;
    }
}

enum StreamAttemptOutcome {
    Completed(Box<LLMResponse>),
    Retry(Duration),
    Failed(LLMError),
    Cancelled,
}

enum StreamResponseOutcome {
    Continue,
    StopReason(acp::StopReason),
}

struct StreamCollectionRequest<'a> {
    turn: &'a PromptExecution,
    span: &'a Span,
    started_at: Instant,
    attempt_index: u32,
    deadlines: &'a mut StreamDeadlineTracker,
    telemetry: &'a mut GenerationTelemetry,
    output: &'a mut ProviderOutput,
}

struct StreamFailureRequest<'a> {
    turn: &'a PromptExecution,
    telemetry: &'a mut GenerationTelemetry,
    output: &'a ProviderOutput,
    attempt_index: u32,
    error: LLMError,
}

struct StreamDeltaRequest<'a> {
    turn: &'a PromptExecution,
    output: &'a mut ProviderOutput,
    deadlines: &'a mut StreamDeadlineTracker,
    telemetry: &'a mut GenerationTelemetry,
    attempt_index: u32,
    delta: String,
    reasoning: bool,
}

enum StreamingTurnOutcome {
    Retry,
    ContinueAfterTools,
    Finished(ProviderTurnOutcome),
}

async fn run_streaming_provider_turn(turn: &mut PromptExecution) -> Result<ProviderTurnOutcome, SdkError> {
    let mut output = ProviderOutput::take_from(turn);
    let mut tool_loop_count = 0usize;
    let mut attempt_index = 0u32;

    loop {
        if turn.session.cancellation.is_cancelled() {
            output.store_in(turn);
            return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
        }
        let request = streaming_request(turn);
        let attempt = collect_streaming_attempt(turn, &request, &mut output, attempt_index).await?;
        match handle_streaming_attempt(turn, &mut output, attempt, attempt_index, &mut tool_loop_count).await? {
            StreamingTurnOutcome::Retry => attempt_index = attempt_index.saturating_add(1),
            StreamingTurnOutcome::ContinueAfterTools => attempt_index = 0,
            StreamingTurnOutcome::Finished(outcome) => {
                if matches!(&outcome, ProviderTurnOutcome::StopReason(_)) {
                    output.store_in(turn);
                }
                return Ok(outcome);
            }
        }
    }
}

async fn handle_streaming_attempt(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    attempt: StreamAttemptOutcome,
    attempt_index: u32,
    tool_loop_count: &mut usize,
) -> Result<StreamingTurnOutcome, SdkError> {
    match attempt {
        StreamAttemptOutcome::Completed(response) => {
            match process_stream_response(turn, output, *response, tool_loop_count).await? {
                StreamResponseOutcome::Continue => Ok(StreamingTurnOutcome::ContinueAfterTools),
                StreamResponseOutcome::StopReason(reason) => {
                    Ok(StreamingTurnOutcome::Finished(ProviderTurnOutcome::StopReason(reason)))
                }
            }
        }
        StreamAttemptOutcome::Retry(delay) => retry_stream_attempt(turn, output, delay, attempt_index).await,
        StreamAttemptOutcome::Failed(error) => {
            let response =
                finish_failed_output(turn, std::mem::replace(output, ProviderOutput::empty()), &error.to_string())
                    .await?;
            Ok(StreamingTurnOutcome::Finished(response))
        }
        StreamAttemptOutcome::Cancelled => {
            Ok(StreamingTurnOutcome::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)))
        }
    }
}

async fn retry_stream_attempt(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    delay: Duration,
    attempt_index: u32,
) -> Result<StreamingTurnOutcome, SdkError> {
    let policy = turn.provider_runtime.retry_policy();
    info!(
        provider = turn.provider_runtime.provider_name(),
        next_attempt = attempt_index + 2,
        retry_count = attempt_index + 1,
        max_retries = policy.max_attempts.saturating_sub(1),
        ?delay,
        "Retrying transient ACP provider stream"
    );
    match cancellable_backoff(delay, &turn.session.cancellation).await {
        Ok(()) => Ok(StreamingTurnOutcome::Retry),
        Err(ProviderCallError::Cancelled) => {
            Ok(StreamingTurnOutcome::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)))
        }
        Err(ProviderCallError::Failed(error)) => {
            let response =
                finish_failed_output(turn, std::mem::replace(output, ProviderOutput::empty()), &error).await?;
            Ok(StreamingTurnOutcome::Finished(response))
        }
    }
}

fn streaming_request(turn: &PromptExecution) -> LLMRequest {
    LLMRequest {
        messages: Arc::new(turn.tool_state.messages.clone()),
        model: turn.session_model.clone(),
        stream: true,
        tools: turn.tool_state.tool_definitions.clone(),
        tool_choice: turn.agent.tool_choice(turn.tool_state.tools_allowed),
        reasoning_effort: turn.reasoning_effort,
        ..Default::default()
    }
}

async fn collect_streaming_attempt(
    turn: &PromptExecution,
    request: &LLMRequest,
    output: &mut ProviderOutput,
    attempt_index: u32,
) -> Result<StreamAttemptOutcome, SdkError> {
    let provider_runtime = &turn.provider_runtime;
    let deadline_policy = provider_runtime.deadline_policy();
    let permit = match provider_runtime.acquire(&turn.session.cancellation).await {
        Ok(permit) => permit,
        Err(ProviderAdmissionError::Cancelled) => return Ok(StreamAttemptOutcome::Cancelled),
        Err(error) => return Err(SdkError::internal_error().data(error.to_string())),
    };
    let mut telemetry = GenerationTelemetry::start(provider_runtime);
    let mut deadlines = StreamDeadlineTracker::new(deadline_policy, Instant::now());
    let started_at = Instant::now();
    let span = tracing::info_span!(
        "acp_provider_stream_attempt",
        operation = "provider_stream_attempt",
        provider = provider_runtime.provider_name(),
        outcome = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    );
    let next_deadline = deadlines.next();
    let stream_result = tokio::select! {
        () = turn.session.cancellation.cancelled() => {
            record_stream_attempt(&span, started_at, "cancelled");
            return Ok(StreamAttemptOutcome::Cancelled);
        }
        phase = sleep_until_stream_deadline(next_deadline) => {
            Err(provider_timeout_error(
                provider_runtime.provider_name(),
                phase,
                phase.timeout(deadline_policy),
            ))
        }
        result = turn.provider.stream(request.clone()).instrument(span.clone()) => result,
    };
    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            record_stream_attempt(&span, started_at, "failure");
            return Ok(classify_stream_failure(StreamFailureRequest {
                turn,
                telemetry: &mut telemetry,
                output,
                attempt_index,
                error,
            }));
        }
    };

    let outcome = collect_stream_events(
        &mut stream,
        StreamCollectionRequest {
            turn,
            span: &span,
            started_at,
            attempt_index,
            deadlines: &mut deadlines,
            telemetry: &mut telemetry,
            output,
        },
    )
    .await;
    drop(stream);
    drop(permit);
    Ok(outcome)
}

async fn collect_stream_events(stream: &mut LLMStream, request: StreamCollectionRequest<'_>) -> StreamAttemptOutcome {
    let StreamCollectionRequest {
        turn,
        span,
        started_at,
        attempt_index,
        deadlines,
        telemetry,
        output,
    } = request;
    let deadline_policy = turn.provider_runtime.deadline_policy();

    loop {
        let next_deadline = deadlines.next();
        let event = tokio::select! {
            () = turn.session.cancellation.cancelled() => {
                record_stream_attempt(span, started_at, "cancelled");
                return StreamAttemptOutcome::Cancelled;
            }
            phase = sleep_until_stream_deadline(next_deadline) => {
                Some(Err(provider_timeout_error(
                    turn.provider_runtime.provider_name(),
                    phase,
                    phase.timeout(deadline_policy),
                )))
            }
            event = poll_provider_stream(stream, span.clone()) => event,
        };
        let Some(event) = event else {
            record_stream_attempt(span, started_at, "failure");
            return classify_stream_failure(StreamFailureRequest {
                turn,
                telemetry,
                output,
                attempt_index,
                error: LLMError::Network {
                    message: "provider stream ended before a completion event".to_string(),
                    metadata: None,
                },
            });
        };

        let event = match event {
            Ok(event) => event,
            Err(error) => {
                record_stream_attempt(span, started_at, "failure");
                return classify_stream_failure(StreamFailureRequest { turn, telemetry, output, attempt_index, error });
            }
        };
        match event {
            LLMStreamEvent::Token { delta } => {
                append_stream_delta(StreamDeltaRequest {
                    turn,
                    output,
                    deadlines,
                    telemetry,
                    attempt_index,
                    delta,
                    reasoning: false,
                })
                .await;
            }
            LLMStreamEvent::Reasoning { delta } => {
                append_stream_delta(StreamDeltaRequest {
                    turn,
                    output,
                    deadlines,
                    telemetry,
                    attempt_index,
                    delta,
                    reasoning: true,
                })
                .await;
            }
            LLMStreamEvent::ReasoningStage { .. } | LLMStreamEvent::ReasoningSignature { .. } => {}
            LLMStreamEvent::Completed { response } => {
                record_stream_attempt(span, started_at, "completion");
                let response = *response;
                if telemetry.first_output_at.is_none() {
                    if let Some(content) = response.content.as_deref() {
                        telemetry.observe_output(&turn.provider_runtime, content, attempt_index);
                    }
                    if let Some(reasoning) = response.reasoning.as_deref() {
                        telemetry.observe_output(&turn.provider_runtime, reasoning, attempt_index);
                    }
                }
                telemetry.complete(&turn.provider_runtime, &response, attempt_index, false);
                emit_response_fallback(turn, output, &response).await;
                return StreamAttemptOutcome::Completed(Box::new(response));
            }
        }
    }
}

fn classify_stream_failure(request: StreamFailureRequest<'_>) -> StreamAttemptOutcome {
    let StreamFailureRequest { turn, telemetry, output, attempt_index, error } = request;
    let policy = turn.provider_runtime.retry_policy();
    let decision = policy.decision_for_llm_error(&error, attempt_index);
    let disposition = if output.emitted_output {
        "partial_output_visible"
    } else {
        retry_disposition(&decision)
    };
    telemetry.failed(&turn.provider_runtime, attempt_index, disposition, &error);
    if output.emitted_output || !decision.retryable {
        StreamAttemptOutcome::Failed(error)
    } else {
        let delay = decision.delay.unwrap_or_else(|| policy.delay_for_attempt(attempt_index));
        StreamAttemptOutcome::Retry(delay)
    }
}

async fn append_stream_delta(request: StreamDeltaRequest<'_>) {
    let StreamDeltaRequest {
        turn,
        output,
        deadlines,
        telemetry,
        attempt_index,
        delta,
        reasoning,
    } = request;
    if delta.is_empty() {
        return;
    }
    output.emitted_output = true;
    deadlines.observe_output();
    telemetry.observe_output(&turn.provider_runtime, &delta, attempt_index);
    let update = if reasoning {
        output.assistant_reasoning.push_str(&delta);
        acp::SessionUpdate::AgentThoughtChunk(text_chunk(delta))
    } else {
        output.assistant_message.push_str(&delta);
        acp::SessionUpdate::AgentMessageChunk(text_chunk(delta))
    };
    drop(turn.agent.send_update(&turn.args.session_id, update).await);
}

async fn emit_response_fallback(turn: &PromptExecution, output: &mut ProviderOutput, response: &LLMResponse) {
    if output.assistant_message.is_empty()
        && let Some(content) = response.content.as_deref().filter(|content| !content.is_empty())
    {
        drop(
            turn.agent
                .send_update(
                    &turn.args.session_id,
                    acp::SessionUpdate::AgentMessageChunk(text_chunk(content.to_string())),
                )
                .await,
        );
        output.assistant_message.push_str(content);
    }
    if output.assistant_reasoning.is_empty()
        && let Some(reasoning) = response.reasoning.as_deref().filter(|reasoning| !reasoning.is_empty())
    {
        drop(
            turn.agent
                .send_update(
                    &turn.args.session_id,
                    acp::SessionUpdate::AgentThoughtChunk(text_chunk(reasoning.to_string())),
                )
                .await,
        );
        output.assistant_reasoning.push_str(reasoning);
    }
}

async fn process_stream_response(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    response: LLMResponse,
    tool_loop_count: &mut usize,
) -> Result<StreamResponseOutcome, SdkError> {
    if turn.tool_state.tools_allowed
        && let Some(tool_calls) = response.tool_calls.clone().filter(|calls| !calls.is_empty())
    {
        let outcome = execute_tool_round(ToolRoundRequest {
            agent: &turn.agent,
            session: &turn.session,
            session_id: &turn.args.session_id,
            tool_calls: &tool_calls,
            assistant_message: &output.assistant_message,
            assistant_reasoning: &output.assistant_reasoning,
            reasoning_details: response.reasoning_details.clone(),
            tool_loop_count,
            streamed: true,
        })
        .await?;
        match outcome {
            ToolRoundOutcome::Cancelled => {
                return Ok(StreamResponseOutcome::StopReason(acp::StopReason::Cancelled));
            }
            ToolRoundOutcome::LimitReached(message) => {
                output.assistant_message = message;
                return Ok(StreamResponseOutcome::StopReason(acp::StopReason::EndTurn));
            }
            ToolRoundOutcome::Continue => {}
        }
        refresh_stream_tool_state(turn).await?;
        output.reset();
        return Ok(StreamResponseOutcome::Continue);
    }

    Ok(StreamResponseOutcome::StopReason(ZedAgent::stop_reason_from_finish(response.finish_reason)))
}

async fn refresh_stream_tool_state(turn: &mut PromptExecution) -> Result<(), SdkError> {
    refresh_resolved_tool_state(turn)?;
    if turn
        .agent
        .maybe_compact_session(CompactionRequest {
            session: &turn.session,
            provider: turn.provider.as_ref(),
            runtime: &turn.provider_runtime,
            model: &turn.session_model,
            tools: turn.tool_state.tool_definitions.as_ref(),
        })
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))?
    {
        turn.tool_state.messages = turn.agent.resolved_messages(&turn.session).map_err(|error| {
            session_access_error(&turn.args.session_id, "resolving compacted session messages", error)
        })?;
    }
    Ok(())
}

enum BufferedGenerationOutcome {
    Response(Box<LLMResponse>),
    Cancelled,
    Failed(String),
}

enum BufferedToolOutcome {
    NoToolCalls,
    Continue,
    Finished(acp::StopReason, Option<String>),
}

enum BufferedResponseOutcome {
    Continue,
    Finished(acp::StopReason),
}

async fn run_buffered_provider_turn(turn: &mut PromptExecution) -> Result<ProviderTurnOutcome, SdkError> {
    let mut output = ProviderOutput::take_from(turn);
    let mut tool_loop_count = 0usize;

    loop {
        if turn.session.cancellation.is_cancelled() {
            output.store_in(turn);
            return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
        }
        let response = match generate_buffered_response(turn).await? {
            BufferedGenerationOutcome::Response(response) => *response,
            BufferedGenerationOutcome::Cancelled => {
                output.store_in(turn);
                return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
            }
            BufferedGenerationOutcome::Failed(error) => {
                return finish_failed_output(turn, output, &error).await;
            }
        };

        if turn.session.cancellation.is_cancelled() {
            output.store_in(turn);
            return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
        }
        emit_response_reasoning(&turn.agent, &turn.args.session_id, &response).await;
        if turn.session.cancellation.is_cancelled() {
            output.store_in(turn);
            return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
        }

        match execute_buffered_tool_calls(turn, &response, &mut tool_loop_count).await? {
            BufferedToolOutcome::Continue => continue,
            BufferedToolOutcome::Finished(stop_reason, message) => {
                if let Some(message) = message {
                    output.assistant_message = message;
                }
                output.store_in(turn);
                return Ok(ProviderTurnOutcome::StopReason(stop_reason));
            }
            BufferedToolOutcome::NoToolCalls => {}
        }

        match process_buffered_response(turn, &mut output, response).await? {
            BufferedResponseOutcome::Continue => continue,
            BufferedResponseOutcome::Finished(stop_reason) => {
                output.store_in(turn);
                return Ok(ProviderTurnOutcome::StopReason(stop_reason));
            }
        }
    }
}

async fn generate_buffered_response(turn: &mut PromptExecution) -> Result<BufferedGenerationOutcome, SdkError> {
    let request = prepare_buffered_request(turn).await?;
    match generate_with_retry(turn.provider.as_ref(), request, &turn.provider_runtime, &turn.session.cancellation).await
    {
        Ok(response) => Ok(BufferedGenerationOutcome::Response(Box::new(response))),
        Err(ProviderCallError::Cancelled) => Ok(BufferedGenerationOutcome::Cancelled),
        Err(ProviderCallError::Failed(error)) => Ok(BufferedGenerationOutcome::Failed(error)),
    }
}

async fn prepare_buffered_request(turn: &mut PromptExecution) -> Result<LLMRequest, SdkError> {
    if turn
        .agent
        .maybe_compact_session(CompactionRequest {
            session: &turn.session,
            provider: turn.provider.as_ref(),
            runtime: &turn.provider_runtime,
            model: &turn.session_model,
            tools: turn.tool_state.tool_definitions.as_ref(),
        })
        .await
        .map_err(|error| SdkError::internal_error().data(error.to_string()))?
    {
        turn.tool_state.messages = turn.agent.resolved_messages(&turn.session).map_err(|error| {
            session_access_error(&turn.args.session_id, "resolving compacted session messages", error)
        })?;
    }

    Ok(LLMRequest {
        messages: Arc::new(turn.tool_state.messages.clone()),
        model: turn.session_model.clone(),
        tools: turn.tool_state.tool_definitions.clone(),
        tool_choice: turn.agent.tool_choice(turn.tool_state.tools_allowed),
        reasoning_effort: turn.reasoning_effort,
        ..Default::default()
    })
}

async fn execute_buffered_tool_calls(
    turn: &mut PromptExecution,
    response: &LLMResponse,
    tool_loop_count: &mut usize,
) -> Result<BufferedToolOutcome, SdkError> {
    if !turn.tool_state.tools_allowed {
        return Ok(BufferedToolOutcome::NoToolCalls);
    }
    let Some(tool_calls) = response.tool_calls.clone().filter(|calls| !calls.is_empty()) else {
        return Ok(BufferedToolOutcome::NoToolCalls);
    };

    let outcome = execute_tool_round(ToolRoundRequest {
        agent: &turn.agent,
        session: &turn.session,
        session_id: &turn.args.session_id,
        tool_calls: &tool_calls,
        assistant_message: response.content.as_deref().unwrap_or_default(),
        assistant_reasoning: "",
        reasoning_details: None,
        tool_loop_count,
        streamed: false,
    })
    .await?;
    match outcome {
        ToolRoundOutcome::Cancelled => Ok(BufferedToolOutcome::Finished(acp::StopReason::Cancelled, None)),
        ToolRoundOutcome::LimitReached(message) => {
            Ok(BufferedToolOutcome::Finished(acp::StopReason::EndTurn, Some(message)))
        }
        ToolRoundOutcome::Continue => {
            refresh_resolved_tool_state(turn)?;
            Ok(BufferedToolOutcome::Continue)
        }
    }
}

fn refresh_resolved_tool_state(turn: &mut PromptExecution) -> Result<(), SdkError> {
    turn.tool_state.messages = turn
        .agent
        .resolved_messages(&turn.session)
        .map_err(|error| session_access_error(&turn.args.session_id, "resolving session messages", error))?;
    turn.tool_state.refresh_availability(
        &turn.agent,
        &turn.session,
        &turn.args.session_id,
        turn.provider_supports_tools,
    )
}

async fn process_buffered_response(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    response: LLMResponse,
) -> Result<BufferedResponseOutcome, SdkError> {
    if let Some(content) = response.content {
        if !content.is_empty() && turn.session.cancellation.is_cancelled() {
            return Ok(BufferedResponseOutcome::Finished(acp::StopReason::Cancelled));
        }
        output.assistant_message = content;
    }

    let stop_reason = ZedAgent::stop_reason_from_finish(response.finish_reason);
    if stop_reason == acp::StopReason::Cancelled {
        return Ok(BufferedResponseOutcome::Finished(stop_reason));
    }
    let Some(reason) = turn
        .agent
        .run_stop_hook(&turn.session, &output.assistant_message, turn.stop_hook_active)
        .await?
    else {
        turn.stop_hook_checked = true;
        return Ok(BufferedResponseOutcome::Finished(stop_reason));
    };

    turn.stop_hook_active = true;
    push_session_message(
        &turn.agent,
        &turn.session,
        &turn.args.session_id,
        Message::assistant(output.assistant_message.clone()),
    )?;
    push_session_message(&turn.agent, &turn.session, &turn.args.session_id, Message::system(reason))?;
    refresh_resolved_tool_state(turn)?;
    output.reset();
    turn.stop_hook_checked = false;
    Ok(BufferedResponseOutcome::Continue)
}

impl ZedAgent {
    async fn run_stop_hook(
        &self,
        session: &SessionHandle,
        assistant_message: &str,
        stop_hook_active: bool,
    ) -> Result<Option<String>, SdkError> {
        let Some(hooks) = session.lifecycle_hooks().map_err(|error| {
            SdkError::internal_error().data(format!("Failed to read lifecycle hooks for ACP Stop hook: {error:#}"))
        })?
        else {
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

fn session_access_error(session_id: &acp::SessionId, operation: &str, error: impl Display) -> SdkError {
    SdkError::internal_error().data(format!("Failed to {operation} for ACP session {session_id}: {error}"))
}

fn push_session_message(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    message: Message,
) -> Result<(), SdkError> {
    agent
        .push_message(session, message)
        .map_err(|error| session_access_error(session_id, "appending a session message", error))
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

fn assistant_tool_call_message(
    content: String,
    tool_calls: Vec<vtcode_core::llm::provider::ToolCall>,
    reasoning: &str,
    reasoning_details: Option<Vec<String>>,
) -> Message {
    let reasoning_details = reasoning_details.map(|details| {
        details
            .into_iter()
            .map(|detail| match serde_json::from_str::<serde_json::Value>(&detail) {
                Ok(value) if value.is_object() || value.is_array() => value,
                _ => serde_json::Value::String(detail),
            })
            .collect()
    });
    let mut message = Message::assistant_with_tools_and_reasoning(content, tool_calls, reasoning_details);
    if !reasoning.is_empty() {
        message.reasoning = Some(reasoning.to_owned());
    }
    message
}

#[cfg(test)]
mod tests {
    use super::CompactionRequest;
    use agent_client_protocol::{Channel, on_receive_notification};
    use assert_fs::TempDir;
    use async_trait::async_trait;
    use proptest::prelude::*;
    use std::collections::BTreeMap;
    use std::fmt::Write as FmtWrite;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::sync::{Mutex as AsyncMutex, mpsc};
    use tracing::field::{Field, Visit};
    use tracing_subscriber::layer::{Context, SubscriberExt};
    use tracing_subscriber::{Layer, Registry};
    use vtcode_config::SubagentDiscoveryInput;
    use vtcode_config::auth::AuthCredentialsStoreMode;
    use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
    use vtcode_core::config::core::PromptCachingConfig;
    use vtcode_core::config::types::{
        AgentConfig as CoreAgentConfig, ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference,
    };
    use vtcode_core::config::{AgentClientProtocolZedConfig, CommandsConfig, ToolsConfig, VTCodeConfig};
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

    #[test]
    fn assistant_tool_call_continuation_preserves_signed_reasoning_details() {
        let signed_reasoning = serde_json::json!({
            "type": "reasoning.encrypted",
            "data": "opaque-reasoning-payload",
            "signature": "provider-signature",
        });
        let array_reasoning = serde_json::json!(["reasoning.encrypted", {"data": "opaque-array-payload"}]);
        let raw_reasoning = "provider-native reasoning detail";
        let message = assistant_tool_call_message(
            "continue with the requested tool".to_string(),
            vec![vtcode_core::llm::provider::ToolCall::function_with_namespace(
                "call-1".to_string(),
                None,
                "read_file".to_string(),
                "{\"path\":\"src/lib.rs\"}".to_string(),
            )],
            "private reasoning",
            Some(vec![
                signed_reasoning.to_string(),
                array_reasoning.to_string(),
                raw_reasoning.to_string(),
            ]),
        );

        assert_eq!(message.reasoning.as_deref(), Some("private reasoning"));
        let archived = serde_json::to_value(message).expect("assistant continuation should serialize");
        assert_eq!(archived["reasoning_details"][0], signed_reasoning);
        assert_eq!(archived["reasoning_details"][1], array_reasoning);
        assert_eq!(archived["reasoning_details"][2], serde_json::json!(raw_reasoning));
    }
    use vtcode_core::llm::provider::LLMError;

    use super::*;

    #[derive(Clone, Default)]
    struct TraceCapture(Arc<Mutex<Vec<String>>>);

    impl<S> Layer<S> for TraceCapture
    where
        S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
            let current = context.lookup_current().map(|span| span.name().to_owned());
            let mut fields = CapturedFields::default();
            event.record(&mut fields);
            let mut line = format!("span={current:?} event={}", event.metadata().name());
            line.push_str(&fields.0);
            self.0.lock().expect("trace capture lock").push(line);
        }

        fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _id: &tracing::Id, _context: Context<'_, S>) {
            let mut fields = CapturedFields::default();
            attrs.record(&mut fields);
            let mut line = format!("span={}", attrs.metadata().name());
            line.push_str(&fields.0);
            self.0.lock().expect("trace capture lock").push(line);
        }

        fn on_record(&self, _id: &tracing::Id, values: &tracing::span::Record<'_>, _context: Context<'_, S>) {
            let mut fields = CapturedFields::default();
            values.record(&mut fields);
            self.0.lock().expect("trace capture lock").push(fields.0);
        }
    }

    #[derive(Default)]
    struct CapturedFields(String);

    impl Visit for CapturedFields {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
        }
    }

    #[test]
    fn stream_poll_runs_inside_safe_attempt_span() {
        let capture = TraceCapture::default();
        let subscriber = Registry::default().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("test runtime should build")
                .block_on(async {
                    let span = tracing::info_span!(
                        "acp_provider_stream_attempt",
                        operation = "provider_stream_attempt",
                        provider = "test-provider",
                        outcome = tracing::field::Empty,
                        elapsed_ms = tracing::field::Empty,
                    );
                    let mut stream = futures::stream::poll_fn(|_context| {
                        tracing::info!(marker = "stream_poll", "polled provider stream");
                        std::task::Poll::Ready(Some(()))
                    });
                    let started_at = Instant::now();
                    assert_eq!(poll_provider_stream(&mut stream, span.clone()).await, Some(()));
                    record_stream_attempt(&span, started_at, "completion");
                });
        });

        let captured = capture.0.lock().expect("trace capture lock").join("\n");
        assert!(captured.contains("span=acp_provider_stream_attempt"));
        assert!(captured.contains("span=Some(\"acp_provider_stream_attempt\")"));
        assert!(captured.contains("marker=\"stream_poll\""));
        assert!(captured.contains("outcome=\"completion\""));
        assert!(captured.contains("elapsed_ms="));
        assert!(!captured.contains("prompt"));
        assert!(!captured.contains("api_key"));
        assert!(!captured.contains("/home/"));
    }

    #[test]
    fn provider_error_telemetry_exposes_stable_network_metadata() {
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
                timeout_phase: None,
            }
        );
    }

    #[test]
    fn provider_error_telemetry_exposes_acp_timeout_phase_separately() {
        let error =
            provider_timeout_error("wire-test", StreamTimeoutPhase::InterTokenIdle, Some(Duration::from_secs(30)));

        assert_eq!(
            provider_error_telemetry(&error),
            ProviderErrorTelemetry {
                code: None,
                status: None,
                timeout_phase: Some("inter_token_idle"),
            }
        );
        assert!(error.to_string().contains("inter-token idle timeout"));
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
        tool_name: &'static str,
        tool_arguments: String,
        additional_tool_calls: Vec<vtcode_core::llm::provider::ToolCall>,
    }

    struct CompactThenAnswerProvider {
        requests: Arc<Mutex<Vec<LLMRequest>>>,
        context_size: usize,
        summary: String,
    }

    struct PendingCompactionProvider {
        started: Arc<tokio::sync::Notify>,
    }

    impl PendingCompactionProvider {
        fn new(started: Arc<tokio::sync::Notify>) -> Self {
            Self { started }
        }
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

        async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
            self.requests.lock().expect("stream requests").push(request);
            let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
            let events = if response_index == 0 {
                let mut tool_calls = vec![vtcode_core::llm::provider::ToolCall::function(
                    "call-tool".to_string(),
                    self.tool_name.to_string(),
                    self.tool_arguments.clone(),
                )];
                tool_calls.extend(self.additional_tool_calls.clone());
                let response = LLMResponse {
                    content: Some("Checking files.".to_string()),
                    reasoning: Some("I need the workspace listing.".to_string()),
                    tool_calls: Some(tool_calls),
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
    impl LLMProvider for CompactThenAnswerProvider {
        fn name(&self) -> &str {
            "wire-test"
        }

        fn effective_context_size(&self, _model: &str) -> usize {
            self.context_size
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

        async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
            self.requests.lock().expect("compaction provider requests").push(request);
            let response = LLMResponse {
                content: Some("Answer after compaction.".to_string()),
                finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            Ok(Box::pin(futures::stream::iter(vec![
                Ok(LLMStreamEvent::Token { delta: "Answer after compaction.".to_string() }),
                Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
            ])))
        }

        async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
            self.requests.lock().expect("compaction provider requests").push(request);
            Ok(LLMResponse::new("wire-model", self.summary.clone()))
        }
    }

    #[async_trait]
    impl LLMProvider for PendingCompactionProvider {
        fn name(&self) -> &str {
            "wire-test"
        }

        fn effective_context_size(&self, _model: &str) -> usize {
            8_192
        }

        fn supported_models(&self) -> Vec<String> {
            vec!["wire-model".to_string()]
        }

        fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
            Ok(())
        }

        async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
            self.started.notify_one();
            std::future::pending().await
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

        async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
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
        build_wire_test_agent_with_options(workspace, None, None).await
    }

    async fn build_wire_test_agent_with_options(
        workspace: &std::path::Path,
        vt_config: Option<&VTCodeConfig>,
        audit_logger: Option<Arc<crate::audit::AcpAuditLogger>>,
    ) -> ZedAgent {
        let audit_logger = match (audit_logger, vt_config) {
            (Some(audit_logger), _) => Some(audit_logger),
            (None, Some(vt_config)) => crate::audit::initialize_acp_audit_logger(vt_config)
                .await
                .expect("initialize ACP audit logger from runtime config"),
            (None, None) => None,
        };
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
            model_behaviour: None,
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
            vt_config,
            audit_logger,
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
            metadata.client_metadata.as_ref().and_then(|meta| meta.get("client")),
            Some(&serde_json::json!("wire-test"))
        );
        assert_eq!(
            metadata.client_metadata.as_ref().and_then(|meta| meta.get("requestId")),
            Some(&serde_json::json!("prompt-1"))
        );
        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&archive_path).expect("read persisted wire ACP session archive"))
                .expect("parse persisted wire ACP session archive");
        assert_eq!(persisted["metadata"]["client_metadata"]["client"], "wire-test");
        assert_eq!(persisted["metadata"]["client_metadata"]["requestId"], "prompt-1");
        std::fs::remove_file(archive_path).expect("remove persisted wire ACP session archive");
    }

    #[tokio::test]
    async fn streamed_tool_calls_audit_success_and_failure_before_streamed_final_answer() {
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
                    tool_name: "list_files",
                    tool_arguments: r#"{"path":""}"#.to_string(),
                    additional_tool_calls: vec![vtcode_core::llm::provider::ToolCall::function(
                        "call-unsupported".to_string(),
                        "unsupported_tool".to_string(),
                        r#"{"probe":"failure"}"#.to_string(),
                    )],
                })
            }),
        );
        let launch_workspace = TempDir::new().expect("wire test launch workspace");
        let workspace = TempDir::new().expect("wire test requested workspace");
        let audit_path = launch_workspace.path().join("tool-audit.jsonl");
        let mut vt_config = VTCodeConfig::default();
        vt_config.acp.audit =
            toml::from_str(&format!("enabled = true\npath = {:?}\nmax_size_bytes = 4096\nmax_files = 2", audit_path))
                .expect("ACP tool audit configuration");
        std::fs::write(workspace.path().join("visible.txt"), "fixture").expect("write workspace fixture");
        std::fs::write(launch_workspace.path().join("wrong-root.txt"), "fixture")
            .expect("write launch workspace fixture");
        let agent = Arc::new(build_wire_test_agent_with_options(launch_workspace.path(), Some(&vt_config), None).await);
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

        let audit_session_id = Arc::new(Mutex::new(None::<String>));
        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |notification: acp::SessionNotification, _cx| {
                    drop(updates_tx.send(notification));
                    Ok(())
                },
                on_receive_notification!(),
            )
            .connect_with(client_channel, {
                let audit_session_id = Arc::clone(&audit_session_id);
                async move |cx: ConnectionTo<Agent>| {
                    drop(
                        cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                            .block_task()
                            .await?,
                    );
                    let session = cx
                        .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                        .block_task()
                        .await?;
                    *audit_session_id.lock().expect("record ACP audit session ID") =
                        Some(session.session_id.to_string());
                    let response = cx
                        .send_request(PromptRequest::new(
                            session.session_id,
                            vec![acp::ContentBlock::Text(acp::TextContent::new("Inspect the workspace"))],
                        ))
                        .block_task()
                        .await?;
                    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                    Ok(())
                }
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
        assert_eq!(
            requests[1].messages.iter().filter(|message| message.is_tool_response()).count(),
            2,
            "the provider must receive both the successful and failed streamed tool outcomes"
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

        let audit_entries = std::fs::read_to_string(&audit_path)
            .expect("read ACP tool audit JSONL")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse ACP tool audit entry"))
            .collect::<Vec<_>>();
        assert_eq!(audit_entries.len(), 2, "each streamed ACP tool execution must be audited");
        assert!(
            audit_entries
                .iter()
                .any(|entry| entry["tool_name"] == "list_files" && entry["status"] == "success")
        );
        assert!(
            audit_entries
                .iter()
                .any(|entry| entry["tool_name"] == "unsupported_tool" && entry["status"] == "failure")
        );
        let session_id = audit_session_id
            .lock()
            .expect("read ACP audit session ID")
            .clone()
            .expect("ACP session ID recorded by the duplex client");
        assert!(audit_entries.iter().all(|entry| entry["session_id"] == session_id));
        assert!(audit_entries.iter().all(|entry| {
            entry["turn_id"].as_str().is_some_and(|turn_id| !turn_id.is_empty())
                && entry["tool_call_id"].as_str().is_some_and(|call_id| !call_id.is_empty())
                && ["arguments_hash", "result_hash"].iter().all(|field| {
                    entry[*field]
                        .as_str()
                        .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
                })
        }));
        assert!(
            audit_entries
                .iter()
                .any(|entry| entry["tool_name"] == "list_files" && entry["tool_call_id"] == "call-tool")
        );
        assert!(
            audit_entries.iter().any(|entry| {
                entry["tool_name"] == "unsupported_tool" && entry["tool_call_id"] == "call-unsupported"
            })
        );
        let turn_id = audit_entries[0]["turn_id"].as_str().expect("audit turn identifier");
        assert!(audit_entries.iter().all(|entry| entry["turn_id"] == turn_id));
        let audit_json = serde_json::to_string(&audit_entries).expect("serialize audited metadata");
        for sensitive_value in [r#""path":"""#, r#""probe":"failure""#, "visible.txt", "wrong-root.txt"] {
            assert!(!audit_json.contains(sensitive_value), "audit output leaked {sensitive_value}");
        }
    }

    #[tokio::test]
    async fn streamed_acp_prompt_compacts_and_checkpoints_session_history() {
        const OLDEST_CONTEXT_MARKER: &str = "acp-oldest-context-marker";
        const SUMMARY_MARKER: &str = "ACP compaction summary marker";

        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory_requests = Arc::clone(&requests);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(CompactThenAnswerProvider {
                    requests: Arc::clone(&factory_requests),
                    context_size: 0,
                    summary: "ACP compaction summary marker".to_string(),
                })
            }),
        );
        let workspace = TempDir::new().expect("wire test compaction workspace");
        let mut vt_config = VTCodeConfig::default();
        vt_config.agent.harness.auto_compaction_enabled = true;
        vt_config.agent.harness.auto_compaction_threshold_tokens = Some(16_384);
        let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await);
        let client_workspace = workspace.path().to_path_buf();
        let (agent_channel, client_channel) = Channel::duplex();

        let agent_connection = install_handlers(
            Agent.builder().name("vtcode-stream-compaction-test"),
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

        let history_agent = Arc::clone(&agent);
        let client_requests = Arc::clone(&requests);
        let client_connection = Client
            .builder()
            .on_receive_notification(
                async move |_notification: acp::SessionNotification, _cx| Ok(()),
                on_receive_notification!(),
            )
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(client_workspace)).block_task().await?;
                let session_handle = history_agent
                    .session_handle(&session.session_id)
                    .expect("ACP session lookup")
                    .expect("new ACP session handle");
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Prompt below the compaction threshold",
                        ))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                {
                    let requests = client_requests.lock().expect("compaction provider requests");
                    assert_eq!(requests.len(), 1, "a below-threshold ACP prompt must not request compaction");
                    assert!(requests[0].stream, "a below-threshold ACP prompt should stream its answer directly");
                }
                for index in 0..12 {
                    let user_context = if index == 0 {
                        format!("old user turn {index} {}", "older context ".repeat(12_000))
                    } else {
                        format!("older user turn {index}")
                    };
                    history_agent
                        .push_message(&session_handle, Message::user(user_context))
                        .expect("append user history");
                    let assistant_context = if index == 0 {
                        format!("{OLDEST_CONTEXT_MARKER} old assistant turn {index}")
                    } else {
                        format!("older assistant turn {index}")
                    };
                    history_agent
                        .push_message(&session_handle, Message::assistant(assistant_context))
                        .expect("append assistant history");
                }

                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Prompt after accumulated history",
                        ))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("ACP compaction prompt should finish")
            .expect("ACP compaction protocol flow should succeed");
        agent_task.abort();
        drop(agent_task.await);

        {
            let requests = requests.lock().expect("compaction provider requests");
            assert_eq!(requests.len(), 3, "the turn must skip small-prompt compaction, then summarize and answer");
            assert!(requests[0].stream, "the below-threshold turn must stream without a summary request");
            assert!(!requests[1].stream, "local compaction must use provider generation for its summary");
            let compaction_request_text = requests[1]
                .messages
                .iter()
                .map(|message| message.content.as_text())
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                compaction_request_text.contains(OLDEST_CONTEXT_MARKER),
                "the compaction request must receive the oldest pre-compaction assistant message"
            );
            assert!(requests[2].stream, "the ACP prompt response must remain streamed after compaction");
            assert!(
                requests[2]
                    .messages
                    .iter()
                    .any(|message| message.content.as_text().contains(SUMMARY_MARKER)),
                "the streamed prompt request must use the generated compaction summary"
            );
            assert!(
                !requests[2]
                    .messages
                    .iter()
                    .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER)),
                "the oldest raw context must be replaced before the provider answer request"
            );
        }

        let session = agent
            .sessions
            .lock()
            .expect("ACP session map")
            .values()
            .next()
            .expect("compacted ACP session")
            .clone();
        let (history, archive_path, session_id) = {
            let data = session.data.lock().expect("compacted ACP session data");
            let archive_path = data
                .archive
                .as_ref()
                .expect("compaction checkpoint archive")
                .path()
                .to_path_buf();
            (data.thread.messages(), archive_path, data.session_id.clone())
        };
        assert!(history.iter().any(|message| message.content.as_text().contains(SUMMARY_MARKER)));
        assert!(
            !history
                .iter()
                .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER))
        );

        let persisted: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&archive_path).expect("read ACP archive after automatic compaction"))
                .expect("parse ACP archive after automatic compaction");
        let persisted_text = persisted.to_string();
        assert!(persisted_text.contains(SUMMARY_MARKER), "the session checkpoint must persist the summary");
        assert!(
            !persisted_text.contains(OLDEST_CONTEXT_MARKER),
            "the session checkpoint must persist compacted history instead of the raw oldest message"
        );

        let restored_agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
        let _load_response = restored_agent
            .load_session(LoadSessionRequest::new(session_id.clone(), workspace.path()))
            .await
            .expect("load compacted ACP session into a fresh runtime");
        let restored_session = restored_agent
            .session_handle(&session_id)
            .expect("look up restored ACP session")
            .expect("loaded ACP session handle");
        let restored_history = restored_session
            .data
            .lock()
            .expect("restored ACP session data")
            .thread
            .messages();
        assert!(
            restored_history
                .iter()
                .any(|message| message.content.as_text().contains(SUMMARY_MARKER)),
            "restored session history must retain the compaction summary"
        );
        assert!(
            !restored_history
                .iter()
                .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER)),
            "restored session history must not bring back compacted source context"
        );
        std::fs::remove_file(archive_path).expect("remove persisted ACP compaction archive");
    }

    async fn large_compaction_session(agent: &ZedAgent, workspace: &std::path::Path) -> SessionHandle {
        let response = agent
            .new_session(NewSessionRequest::new(workspace))
            .await
            .expect("create ACP compaction session");
        let session = agent
            .session_handle(&response.session_id)
            .expect("ACP session lookup")
            .expect("new ACP session handle");
        for index in 0..12 {
            agent
                .push_message(
                    &session,
                    Message::user(format!("large user turn {index} {}", "older context ".repeat(12_000))),
                )
                .expect("append large user history");
            agent
                .push_message(&session, Message::assistant(format!("large assistant turn {index}")))
                .expect("append large assistant history");
        }
        session
    }

    fn compaction_config(enabled: bool) -> VTCodeConfig {
        let mut config = VTCodeConfig::default();
        config.agent.harness.auto_compaction_enabled = enabled;
        config.agent.harness.auto_compaction_threshold_tokens = Some(1);
        config
    }

    #[tokio::test]
    async fn prompt_over_budget_fails_closed_when_compaction_makes_no_change() {
        use assert_fs::TempDir;

        let workspace = TempDir::new().expect("temporary compaction workspace");
        let config = compaction_config(false);
        let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
        let session = large_compaction_session(&agent, workspace.path()).await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = CompactThenAnswerProvider {
            requests: Arc::clone(&requests),
            context_size: 8_192,
            summary: "unused summary".to_string(),
        };
        let runtime = agent.provider_runtime.for_provider("wire-test");

        let error = agent
            .maybe_compact_session(CompactionRequest {
                session: &session,
                provider: &provider,
                runtime: &runtime,
                model: "wire-model",
                tools: None,
            })
            .await
            .expect_err("over-budget prompt must not continue when compaction is disabled");

        assert!(error.to_string().contains("automatic compaction made no change"));
        assert!(requests.lock().expect("compaction requests").is_empty());
    }

    #[tokio::test]
    async fn prompt_over_budget_fails_closed_when_compaction_stays_over_budget() {
        use assert_fs::TempDir;

        let workspace = TempDir::new().expect("temporary compaction workspace");
        let config = compaction_config(true);
        let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
        let session = large_compaction_session(&agent, workspace.path()).await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let provider = CompactThenAnswerProvider {
            requests: Arc::clone(&requests),
            context_size: 8_192,
            summary: "oversized summary ".repeat(12_000),
        };
        let runtime = agent.provider_runtime.for_provider("wire-test");

        let error = agent
            .maybe_compact_session(CompactionRequest {
                session: &session,
                provider: &provider,
                runtime: &runtime,
                model: "wire-model",
                tools: None,
            })
            .await
            .expect_err("over-budget compacted prompt must not continue to the provider answer");

        assert!(error.to_string().contains("still exceeds its safe provider admission budget"));
        let requests = requests.lock().expect("compaction requests");
        assert_eq!(requests.len(), 1, "only the compaction request may reach the provider");
        assert!(!requests[0].stream, "the provider request must be compaction, not an answer turn");
    }

    #[tokio::test]
    async fn cancelled_acp_compaction_aborts_the_pending_provider_request() {
        use assert_fs::TempDir;

        let workspace = TempDir::new().expect("temporary compaction workspace");
        let config = compaction_config(true);
        let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await);
        let session = large_compaction_session(&agent, workspace.path()).await;
        let started = Arc::new(tokio::sync::Notify::new());
        let provider = Arc::new(PendingCompactionProvider::new(Arc::clone(&started)));
        let runtime = agent.provider_runtime.for_provider("wire-test");
        let task = {
            let agent = Arc::clone(&agent);
            let session = session.clone();
            let provider = Arc::clone(&provider);
            tokio::spawn(async move {
                agent
                    .maybe_compact_session(CompactionRequest {
                        session: &session,
                        provider: provider.as_ref(),
                        runtime: &runtime,
                        model: "wire-model",
                        tools: None,
                    })
                    .await
            })
        };

        tokio::time::timeout(Duration::from_secs(5), started.notified())
            .await
            .expect("provider compaction request should start");
        session.cancellation.cancel();
        let error = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("cancellation should stop the compaction task")
            .expect("compaction task should not panic")
            .expect_err("cancelled compaction must not report success");

        assert!(error.to_string().contains("ACP compaction cancelled"));
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
                    tool_name: "task_tracker",
                    tool_arguments: task_arguments.clone(),
                    additional_tool_calls: Vec::new(),
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
                    tool_name: "apply_patch",
                    tool_arguments: patch_arguments.clone(),
                    additional_tool_calls: Vec::new(),
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
    async fn acp_local_exec_preserves_public_command_heredoc() {
        let workspace = TempDir::new().expect("wire test workspace");
        let agent = build_wire_test_agent(workspace.path()).await;
        let heredoc = "cat <<'EOF'\nACP heredoc output\nEOF";

        for args in [
            serde_json::json!({"cmd": heredoc}),
            serde_json::json!({
                "cmd": heredoc,
                "justification": "exercise ACP heredoc argument routing"
            }),
        ] {
            let report = agent
                .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-heredoc")
                .await;

            assert_eq!(
                report.status,
                acp::ToolCallStatus::Completed,
                "heredoc public command failed: {}",
                report.llm_response
            );
            let payload: serde_json::Value =
                serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
            let output = payload["result"]
                .get("output")
                .or_else(|| payload["result"].get("stdout"))
                .and_then(serde_json::Value::as_str)
                .expect("successful command response must contain text output");
            assert_eq!(output, "ACP heredoc output\n");
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
            Arc::new(vtcode_core::metrics::MetricsCollector::new()),
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
            Arc::new(vtcode_core::metrics::MetricsCollector::new()),
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
