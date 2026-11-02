//! Coordinates ACP prompt preparation, hooks, compaction, and provider-turn execution.
use super::super::log_suppressed_error;
use super::super::tool_config::SessionToolDefinitionRequest;
use super::super::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use super::provider::IncompleteProviderTurn;
use super::*;

mod buffered;
mod cancelled;
mod entry;
mod finish;
#[cfg(test)]
mod flush_tests;
#[cfg(test)]
mod gate_clock_tests;
mod input;
mod lifecycle;
mod outcome;
mod output;
#[cfg(test)]
mod prompt_outcome_tests;
mod provider_context;
mod streaming;

pub(super) use buffered::response_reasoning_update;
#[cfg(test)]
pub(crate) use cancelled::commit_cancelled_draft_for_test;
pub(super) use cancelled::{finish_cancelled_output, finish_failed_or_cancelled_output};
#[cfg(test)]
pub(super) use entry::run_prompt_with_metric_sink;
pub(super) use entry::{gate_wait_ms, run_prompt, run_prompt_with_retry_observer};
pub(super) use finish::{
    finish_failed_output, finish_failed_provider_turn, persist_session_checkpoint, require_session_checkpoint,
};
pub(super) use outcome::prompt_outcome;
pub(super) use output::{ProviderOutput, assistant_tool_call_message};

pub(super) struct TurnGuard {
    thread: ThreadRuntimeHandle,
}

impl TurnGuard {
    /// Starts the turn guard that rejects a concurrent turn on the thread.
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
    /// Builds the per-turn tool state before provider capabilities are known.
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

    /// Re-reads session configuration and refreshes tool availability after
    /// a tool round or provider switch.
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

/// Executes one provider-requested tool round and commits its recovery data.
///
/// A cancellation observed before or after execution returns
/// [`ToolRoundOutcome::Cancelled`]; any other failure becomes the sanitized
/// `tool_execution_failed` error.
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

/// Resolves provider runtime, tool state and streaming policy for a prompt.
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

impl PromptExecution {
    /// Executes the provider turn and records its bounded outcome.
    ///
    /// This is the only path that labels an outcome with the provider mode;
    /// prompts that finish during admission are counted by the caller under
    /// the fixed pre-initialization mode.
    async fn run(self, retry_observed_at: &provider::RetryObservationClock<'_>) -> Result<PromptResponse, SdkError> {
        let metric_sink = self.metric_sink.clone();
        let mode = if self.allow_streaming {
            ProviderMode::Stream
        } else {
            ProviderMode::Buffered
        };
        let result = self.run_turn(retry_observed_at).await;
        metric_sink
            .record_prompt(PromptMetricObservation::Outcome { mode, outcome: prompt_outcome(&result) })
            .await;
        result
    }

    /// Drives one provider turn and finalizes its outcome.
    ///
    /// Cancellation is re-checked after the last await so a cancel that lands
    /// during finalization still answers `Cancelled`.
    async fn run_turn(
        mut self,
        retry_observed_at: &provider::RetryObservationClock<'_>,
    ) -> Result<PromptResponse, SdkError> {
        let cancellation = self.session.cancellation.clone();
        let outcome = if self.allow_streaming {
            streaming::run_streaming_provider_turn(&mut self, retry_observed_at).await?
        } else {
            buffered::run_buffered_provider_turn(&mut self, retry_observed_at).await?
        };
        let result = match outcome {
            ProviderTurnOutcome::StopReason(stop_reason) => self.finish(stop_reason).await,
            ProviderTurnOutcome::Response(response) => Ok(response),
        };
        // Finalizing a turn awaits checkpoint persistence. A `session/cancel`
        // that arrives during that window must still win: ACP requires the
        // prompt to answer `Cancelled` once cancellation has been observed, so
        // re-check after the last await rather than returning a stale
        // pre-cancel result to the client and the gate. This covers outcomes
        // that never reached the cancelled path, including a failure whose
        // notice and history were already committed.
        if cancellation.is_cancelled() {
            return Ok(PromptResponse::new(acp::StopReason::Cancelled));
        }
        result
    }
}

/// Builds the sanitized internal error for a session-state access failure.
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

/// Appends a message to durable session history, mapping storage errors to a
/// sanitized session-access failure.
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

/// Buffered turns emit the completed draft as one final chunk; streaming turns
/// already sent it.
pub(super) fn should_emit_buffered_final_chunk(allow_streaming: bool) -> bool {
    !allow_streaming
}
