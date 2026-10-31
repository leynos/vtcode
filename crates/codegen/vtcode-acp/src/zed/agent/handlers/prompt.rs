//! Coordinates ACP prompt preparation, hooks, compaction, and provider-turn execution.
use super::super::log_suppressed_error;
use super::super::tool_config::SessionToolDefinitionRequest;
use super::super::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use super::provider::IncompleteProviderTurn;
use super::*;

mod buffered;
mod cancelled;
mod finish;
#[cfg(test)]
mod flush_tests;
mod input;
mod lifecycle;
mod output;
mod provider_context;
mod streaming;

pub(super) use buffered::response_reasoning_update;
pub(super) use cancelled::{finish_cancelled_output, finish_failed_or_cancelled_output};
pub(super) use finish::{
    finish_failed_output, finish_failed_provider_turn, persist_session_checkpoint, require_session_checkpoint,
};
pub(super) use output::{ProviderOutput, assistant_tool_call_message};

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

pub(super) struct ToolRoundRequest<'a> {
    pub(super) agent: &'a ZedAgent,
    pub(super) session: &'a SessionHandle,
    pub(super) session_id: &'a acp::SessionId,
    pub(super) tool_calls: &'a [LLMToolCall],
    pub(super) assistant_message: &'a str,
    pub(super) assistant_reasoning: &'a str,
    pub(super) reasoning_details: Option<Vec<String>>,
    pub(super) tool_loop_count: &'a mut usize,
    pub(super) streamed: bool,
}

pub(super) enum ToolRoundOutcome {
    Continue,
    Cancelled,
    LimitReached(String),
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
    /// Whether at least one assistant `AgentMessageChunk` reached the client
    /// during this turn. Thought chunks and tool updates do not count: a turn
    /// that ends without visible assistant content is a failure, not a silent
    /// `EndTurn`.
    assistant_emitted: bool,
    stop_hook_active: bool,
    stop_hook_checked: bool,
}

pub(super) async fn execute_tool_round(request: ToolRoundRequest<'_>) -> Result<ToolRoundOutcome, SdkError> {
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
    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    drop(stage_thread_tool_calls(&thread, assistant_tool_message));
    let tool_call_count = tool_calls.len();
    info!(session_id = %session_id, tool_call_count, tool_loop_count = *tool_loop_count,
          tool_recovery_outcome = "placeholders_staged", "ACP tool recovery placeholders staged");
    require_session_checkpoint(agent, session, "assistant_tool_calls_write_ahead").await?;

    let tool_results = match agent.execute_tool_calls(session, session_id, tool_calls).await {
        Ok(results) => results,
        Err(error) => {
            warn!(%error, session_id = %session_id, tool_call_count,
                  tool_recovery_outcome = "execution_interrupted", "Tool execution failed");
            persist_session_checkpoint(agent, session, "interrupted_tool_results").await;
            if session.cancellation.is_cancelled() {
                return Ok(ToolRoundOutcome::Cancelled);
            }
            return Err(tool_execution_failed_error());
        }
    };

    let finalized_tool_count = replace_thread_tool_results(&thread, &tool_results);
    info!(session_id = %session_id, tool_call_count, tool_loop_count = *tool_loop_count,
          finalized_tool_count, tool_recovery_outcome = "results_replaced", "ACP tool recovery results replaced");
    persist_session_checkpoint(agent, session, "tool_results").await;

    Ok(if session.cancellation.is_cancelled() {
        ToolRoundOutcome::Cancelled
    } else {
        ToolRoundOutcome::Continue
    })
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
        assistant_emitted: false,
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
    let session = agent
        .session_handle(&args.session_id)
        .map_err(|error| session_access_error(&args.session_id, "looking up session", error))?
        .ok_or_else(|| SdkError::invalid_params().data(json!({ "reason": "unknown_session" })))?;
    // ACP lets a client steer an active turn by sending another
    // `session/prompt`. Serialize the turns per session instead of rejecting
    // the steer or racing the active turn's state; this is only awaited here,
    // inside the spawned prompt task.
    //
    // Sample the admitted count before waiting: it is the queue depth this
    // prompt joined, including any turn already holding the gate. Reporting it
    // alongside the wait keeps a stalled turn visible without capping steers,
    // which ACP deliberately allows.
    let queued_prompts = session.admitted_prompt_count().saturating_sub(1);
    let gate_wait_started = Instant::now();
    if queued_prompts > 0 {
        // Emit before waiting: a turn stalled on the gate must be visible
        // while it is stalled, not only once it finally runs.
        info!(
            session_id = %args.session_id,
            queued_prompts,
            "Waiting for the ACP prompt gate"
        );
    }
    let _prompt_gate = session.acquire_prompt_gate().await;
    debug!(
        session_id = %args.session_id,
        queued_prompts,
        wait_ms = u64::try_from(gate_wait_started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "Acquired ACP prompt gate"
    );
    // Clear cancellation only once the previous turn has fully completed. A
    // queued steer must not clear a cancel aimed at the turn it waited for.
    if !session.cancellation.reset() {
        return Ok(PromptResponse::new(acp::StopReason::Cancelled));
    }
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
pub(super) fn session_access_error(session_id: &acp::SessionId, operation: &str, error: impl Display) -> SdkError {
    SdkError::internal_error().data(format!("Failed to {operation} for ACP session {session_id}: {error}"))
}

/// A non-cancelled turn completed without ever sending the client assistant
/// content. Returning `EndTurn` here would acknowledge a prompt with no
/// response, so surface an explicit, sanitized failure instead.
pub(super) fn empty_response_error() -> SdkError {
    SdkError::internal_error().data(json!({
        "reason": "empty_response",
        "message": "The provider completed the turn without emitting assistant content.",
    }))
}

/// Tool execution failed before producing results. The underlying error stays
/// in logs; the client receives a stable, sanitized provider-failure shape.
pub(super) fn tool_execution_failed_error() -> SdkError {
    SdkError::internal_error().data(json!({
        "reason": "tool_execution_failed",
        "message": "Tool execution failed before producing results.",
    }))
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
