//! Coordinates ACP prompt preparation, hooks, compaction, and provider-turn execution.
use super::super::log_suppressed_error;
use super::super::tool_config::SessionToolDefinitionRequest;
use super::super::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use super::provider::{AttemptFailureFuture, AttemptFailureSink, IncompleteProviderTurn};
use super::*;

/// Routing for one session's provider attempt failures into ACP rate-limit
/// notices.
///
/// This is the ACP-boundary implementation of [`AttemptFailureSink`]: retry
/// orchestration names only the port, so notice construction and delivery stay
/// with the rest of the prompt presentation logic.
pub(super) struct RateLimitNoticeSink<'a> {
    agent: &'a ZedAgent,
    session_id: &'a acp::SessionId,
}

impl<'a> RateLimitNoticeSink<'a> {
    /// Bind the sink to one session's ACP client surface.
    pub(super) fn new(agent: &'a ZedAgent, session_id: &'a acp::SessionId) -> Self {
        Self { agent, session_id }
    }
}

impl AttemptFailureSink for RateLimitNoticeSink<'_> {
    /// Publish the failed attempt as a rate-limit notice for this session.
    fn announce<'a>(
        &'a self,
        provider: &'a str,
        error: &'a LLMError,
        retry_delay: Option<Duration>,
    ) -> AttemptFailureFuture<'a> {
        Box::pin(async move {
            self.agent
                .publish_rate_limit_notice(self.session_id, provider, error, retry_delay)
                .await;
        })
    }
}

mod buffered;
mod input;
mod lifecycle;
mod provider_context;
mod streaming;

pub(super) use buffered::response_reasoning_update;

/// Persist the session archive and refresh its transcript path, logging failures.
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

/// Emit the partial answer and return an incomplete-turn response.
pub(super) async fn finish_failed_provider_turn(
    turn: &PromptExecution,
    error: &str,
) -> Result<PromptResponse, SdkError> {
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

/// Store partial provider output on the turn, then finish it as failed.
pub(super) async fn finish_failed_output(
    turn: &mut PromptExecution,
    output: ProviderOutput,
    error: &str,
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
    /// Claim the thread's turn slot and reject a concurrent prompt.
    pub(super) fn begin(thread: ThreadRuntimeHandle) -> Result<Self, SdkError> {
        let _submission_id = thread.begin_turn().map_err(|error| {
            warn!(thread_id = %thread.thread_id(), prompt_outcome = "turn_in_progress_rejected", "Rejected concurrent ACP prompt");
            SdkError::internal_error().data(json!({ "reason": "turn_in_progress", "detail": error.to_string() }))
        })?;
        Ok(Self { thread })
    }
}

impl Drop for TurnGuard {
    /// Release the thread's turn slot.
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
    /// Create tool state for a prompt that has not resolved tools yet.
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

    /// Re-resolve tool availability and definitions from the session.
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

/// Stage and execute one round of assistant tool calls with checkpoints.
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
            return Err(error);
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

/// Build the provider and tool state that will drive the prompt.
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

/// Prepare input, accept the user prompt, and run the provider turn.
pub(super) async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
    let input = match input::prepare_prompt_input(agent, args).await? {
        PromptInputOutcome::Response(response) => return Ok(response),
        PromptInputOutcome::Ready(input) => input,
    };
    if !input::accept_user_prompt(&input).await? {
        return Ok(PromptResponse::new(acp::StopReason::Refusal));
    }
    initialize_prompt_execution(input).await?.run().await
}

impl PromptExecution {
    /// Run the streaming or buffered provider turn and finish it.
    async fn run(mut self) -> Result<PromptResponse, SdkError> {
        let outcome = if self.allow_streaming {
            streaming::run_streaming_provider_turn(&mut self).await?
        } else {
            buffered::run_buffered_provider_turn(&mut self).await?
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
    /// Create an output buffer with no visible text.
    pub(super) fn empty() -> Self {
        Self {
            assistant_message: String::new(),
            assistant_reasoning: String::new(),
            emitted_output: false,
        }
    }

    /// Move the turn's buffered output into a new provider output.
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

    /// Clear buffered text and discard any emission marker.
    pub(super) fn reset(&mut self) {
        self.assistant_message.clear();
        self.assistant_reasoning.clear();
        self.emitted_output = false;
    }

    /// Copy this output back into the turn's buffers.
    pub(super) fn store_in(self, turn: &mut PromptExecution) {
        turn.assistant_message = self.assistant_message;
        turn.assistant_reasoning = self.assistant_reasoning;
    }
}

/// Build an internal error naming the session operation that failed.
pub(super) fn session_access_error(session_id: &acp::SessionId, operation: &str, error: impl Display) -> SdkError {
    SdkError::internal_error().data(format!("Failed to {operation} for ACP session {session_id}: {error}"))
}

/// Append a message to the session, mapping failures to a session error.
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

/// Report whether the final chunk is emitted from the buffered path.
pub(super) fn should_emit_buffered_final_chunk(allow_streaming: bool) -> bool {
    !allow_streaming
}

/// Build an assistant message that keeps tool calls and signed reasoning details.
pub(super) fn assistant_tool_call_message(
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

/// Persists the strict write-ahead checkpoint required before tool execution.
///
/// Ordinary transcript checkpoints remain best-effort through
/// [`persist_session_checkpoint`].
async fn require_session_checkpoint(
    agent: &ZedAgent,
    session: &SessionHandle,
    boundary: &'static str,
) -> Result<(), SdkError> {
    if let Err(error) = agent.checkpoint_session(session).await {
        log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
        warn!(
            operation = "session_checkpoint",
            error_category = "archive_write_failed",
            boundary,
            "Failed to persist ACP session checkpoint"
        );
        return Err(
            SdkError::internal_error().data(format!("Failed to persist ACP session checkpoint at {boundary}: {error}"))
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
    Ok(())
}
