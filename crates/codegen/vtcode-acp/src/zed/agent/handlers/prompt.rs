//! Coordinates ACP prompt preparation, hooks, compaction, and provider-turn execution.
use super::super::log_suppressed_error;
use super::super::tool_config::SessionToolDefinitionRequest;
use super::provider::IncompleteProviderTurn;
use super::*;

mod buffered;
mod input;
mod lifecycle;
mod provider_context;
mod streaming;
mod tool_round;

pub(super) use tool_round::{ToolRoundOutcome, ToolRoundRequest, assistant_tool_call_message, execute_tool_round};

#[tracing::instrument(skip_all, fields(boundary = boundary))]
pub(super) async fn persist_session_checkpoint(agent: &ZedAgent, session: &SessionHandle, boundary: &'static str) {
    if let Err(error) = agent.checkpoint_session(session).await {
        log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
        warn!(
            operation = "session_checkpoint",
            error_category = "archive_write_failed",
            boundary,
            "Failed to persist ACP session checkpoint"
        );
    }
    if let Err(error) = session.update_transcript_path().await {
        log_suppressed_error(&error, "transcript_update", "transcript_write_failed");
        warn!(
            operation = "transcript_update",
            error_category = "transcript_write_failed",
            boundary,
            "Failed to update ACP session transcript path"
        );
    }
}

pub(super) async fn finish_failed_provider_turn(
    turn: &PromptExecution,
    error: SafeProviderProjection,
) -> Result<PromptResponse, SdkError> {
    let IncompleteProviderTurn { message, visible_update, response } =
        IncompleteProviderTurn::from_failure(&turn.assistant_message, &turn.assistant_reasoning, error);
    if let Err(error) = turn
        .agent
        .send_update(&turn.args.session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(visible_update)))
        .await
    {
        warn!(
            session_id = %turn.args.session_id,
            update_kind = "agent_message_chunk",
            error = %error,
            "Failed to send assistant update to ACP client"
        );
    }
    push_session_message(&turn.agent, &turn.session, &turn.args.session_id, message)?;
    persist_session_checkpoint(&turn.agent, &turn.session, "incomplete_provider_turn").await;
    warn!(
        error_category = error.category(),
        error_status = ?error.status(),
        partial_text_bytes = turn.assistant_message.len(),
        partial_reasoning_bytes = turn.assistant_reasoning.len(),
        "ACP provider failed to complete the turn"
    );
    Ok(response)
}

pub(super) async fn finish_failed_output(
    turn: &mut PromptExecution,
    output: ProviderOutput,
    error: SafeProviderProjection,
) -> Result<ProviderTurnOutcome, SdkError> {
    output.store_in(turn);
    finish_failed_provider_turn(turn, error)
        .await
        .map(ProviderTurnOutcome::Response)
}

pub(super) struct TurnGuard {
    thread: ThreadRuntimeHandle,
}

impl TurnGuard {
    pub(super) fn begin(thread: ThreadRuntimeHandle) -> Result<Self, SdkError> {
        let _submission_id = thread.begin_turn().map_err(|error| {
            warn!(thread_id = %thread.thread_id(), prompt_outcome = "turn_in_progress_rejected", "Rejected concurrent ACP prompt");
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

pub(super) struct PromptToolState {
    enabled_tools: Vec<SupportedTool>,
    primary_agent: String,
    has_local_tools: bool,
    tools_allowed: bool,
    tool_definitions: Option<Arc<Vec<ToolDefinition>>>,
    messages: Vec<Message>,
}

impl PromptToolState {
    pub(super) fn new(enabled_tools: Vec<SupportedTool>, primary_agent: String, provider_supports_tools: bool) -> Self {
        Self {
            enabled_tools,
            primary_agent,
            has_local_tools: false,
            tools_allowed: provider_supports_tools,
            tool_definitions: None,
            messages: Vec::new(),
        }
    }

    pub(super) fn refresh_availability(
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
            .session_tool_definitions(SessionToolDefinitionRequest {
                session,
                provider_supports_tools,
                enabled_tools: &self.enabled_tools,
                primary_agent: &self.primary_agent,
            })
            .map_err(|error| session_access_error(session_id, "resolving tool definitions", error))?
            .map(Arc::new);
        Ok(())
    }
}

pub(super) struct PromptInput {
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    session: SessionHandle,
    _turn_guard: TurnGuard,
    user_message: String,
}

pub(super) enum PromptInputOutcome {
    Response(PromptResponse),
    Ready(PromptInput),
}

pub(super) enum ProviderTurnOutcome {
    StopReason(acp::StopReason),
    Response(PromptResponse),
}

pub(super) struct PromptExecution {
    metric_sink: ProviderMetricSink,
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
    provider_iteration: usize,
    stop_hook_active: bool,
    stop_hook_checked: bool,
}

async fn initialize_prompt_execution(input: PromptInput) -> Result<PromptExecution, SdkError> {
    let provider = provider_context::create_prompt_provider_context(&input)?;
    let tool_state = provider_context::initialize_prompt_tool_state(&input, &provider).await?;
    let has_stop_hooks = input
        .session
        .has_stop_hooks()
        .map_err(|error| session_access_error(&input.args.session_id, "checking Stop hooks", error))?;
    let allow_streaming = should_allow_streaming(provider.supports_streaming, tool_state.tools_allowed, has_stop_hooks);
    let PromptInput { agent, args, session, _turn_guard, user_message: _ } = input;

    Ok(PromptExecution {
        metric_sink: ProviderMetricSink::production(),
        agent,
        args,
        session,
        _turn_guard,
        provider: provider.provider,
        provider_runtime: provider.runtime,
        session_model: provider.model,
        reasoning_effort: provider.reasoning_effort,
        provider_supports_tools: provider.supports_tools,
        allow_streaming,
        tool_state,
        assistant_message: String::with_capacity(4096),
        assistant_reasoning: String::with_capacity(2048),
        provider_iteration: 0,
        stop_hook_active: false,
        stop_hook_checked: false,
    })
}

pub(super) struct PromptProviderContext {
    provider: Box<dyn LLMProvider>,
    runtime: ProviderRequestRuntime,
    model: String,
    reasoning_effort: Option<ReasoningEffortLevel>,
    supports_streaming: bool,
    supports_tools: bool,
}

pub(super) async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
    run_prompt_with_retry_observer(agent, args, SystemTime::now, ProviderMetricSink::production()).await
}

#[cfg(test)]
pub(super) async fn run_prompt_with_metric_sink(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    metric_sink: ProviderMetricSink,
) -> Result<PromptResponse, SdkError> {
    run_prompt_with_retry_observer(agent, args, SystemTime::now, metric_sink).await
}

pub(super) async fn run_prompt_with_retry_observer(
    agent: Arc<ZedAgent>,
    args: PromptRequest,
    retry_observed_at: impl Fn() -> SystemTime + Send + Sync,
    metric_sink: ProviderMetricSink,
) -> Result<PromptResponse, SdkError> {
    let input = match input::prepare_prompt_input(agent, args).await? {
        PromptInputOutcome::Response(response) => return Ok(response),
        PromptInputOutcome::Ready(input) => input,
    };
    if !input::accept_user_prompt(&input).await? {
        return Ok(PromptResponse::new(acp::StopReason::Refusal));
    }
    let mut execution = initialize_prompt_execution(input).await?;
    execution.metric_sink = metric_sink;
    execution.run(&retry_observed_at).await
}

impl PromptExecution {
    async fn run(
        mut self,
        retry_observed_at: &provider::RetryObservationClock<'_>,
    ) -> Result<PromptResponse, SdkError> {
        let outcome = if self.allow_streaming {
            streaming::run_streaming_provider_turn(&mut self, retry_observed_at).await?
        } else {
            buffered::run_buffered_provider_turn(&mut self, retry_observed_at).await?
        };
        match outcome {
            ProviderTurnOutcome::StopReason(stop_reason) => self.finish(stop_reason).await,
            ProviderTurnOutcome::Response(response) => Ok(response),
        }
    }
}
pub(super) struct ProviderOutput {
    assistant_message: String,
    assistant_reasoning: String,
    emitted_output: bool,
}

impl ProviderOutput {
    pub(super) fn empty() -> Self {
        Self {
            assistant_message: String::new(),
            assistant_reasoning: String::new(),
            emitted_output: false,
        }
    }

    pub(super) fn take_from(turn: &mut PromptExecution) -> Self {
        let assistant_message = std::mem::take(&mut turn.assistant_message);
        let assistant_reasoning = std::mem::take(&mut turn.assistant_reasoning);
        let emitted_output = !assistant_message.is_empty() || !assistant_reasoning.is_empty();
        Self {
            assistant_message,
            assistant_reasoning,
            emitted_output,
        }
    }

    pub(super) fn reset(&mut self) {
        self.assistant_message.clear();
        self.assistant_reasoning.clear();
        self.emitted_output = false;
    }

    pub(super) fn store_in(self, turn: &mut PromptExecution) {
        turn.assistant_message = self.assistant_message;
        turn.assistant_reasoning = self.assistant_reasoning;
    }
}

pub(super) fn session_access_error(session_id: &acp::SessionId, operation: &str, error: impl Display) -> SdkError {
    SdkError::internal_error().data(format!("Failed to {operation} for ACP session {session_id}: {error}"))
}

pub(super) fn push_session_message(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    message: Message,
) -> Result<(), SdkError> {
    agent
        .push_message(session, message)
        .map_err(|error| session_access_error(session_id, "appending a session message", error))
}

/// Streaming is safe unless a Stop hook needs to inspect the complete draft.
/// Other lifecycle hooks do not affect whether ACP can receive streamed text.
pub(super) fn should_allow_streaming(supports_streaming: bool, _tools_allowed: bool, has_stop_hooks: bool) -> bool {
    supports_streaming && !has_stop_hooks
}

pub(super) fn should_emit_buffered_final_chunk(allow_streaming: bool) -> bool {
    !allow_streaming
}
