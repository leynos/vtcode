//! Coordinates ACP prompt preparation, hooks, compaction, and provider-turn execution.
use super::super::tool_config::SessionToolDefinitionRequest;
use super::provider::{
    GenerationTelemetry, IncompleteProviderTurn, ProviderCallError, StreamDeadlineTracker, StreamTimeoutPhase,
    cancellable_backoff, create_prompt_provider,
};
use super::*;

mod buffered;
mod streaming;

pub(super) use buffered::response_reasoning_update;

#[tracing::instrument(skip_all, fields(boundary = boundary))]
pub(super) async fn persist_session_checkpoint(agent: &ZedAgent, session: &SessionHandle, boundary: &'static str) {
    if let Err(error) = agent.checkpoint_session(session).await {
        warn!(%error, boundary, "Failed to persist ACP session checkpoint");
    }
    if let Err(error) = session.update_transcript_path().await {
        warn!(%error, boundary, "Failed to update ACP session transcript path");
    }
}

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
    pub(super) fn begin(thread: ThreadRuntimeHandle) -> Result<Self, SdkError> {
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
    if let Some(reason) = apply_user_prompt_hooks(input).await? {
        warn!(%reason, "ACP UserPromptSubmit hook blocked prompt");
        return Ok(false);
    }

    push_session_message(agent, session, &args.session_id, Message::user(user_message.clone()))?;
    agent
        .merge_session_acp_meta(session, args.meta.clone())
        .map_err(|error| session_access_error(&args.session_id, "merging client metadata", error))?;
    persist_session_checkpoint(agent, session, "user_message").await;
    Ok(true)
}

async fn apply_user_prompt_hooks(input: &PromptInput) -> Result<Option<String>, SdkError> {
    let PromptInput { agent, args, session, user_message, .. } = input;
    let Some(hooks) = session
        .lifecycle_hooks()
        .map_err(|error| session_access_error(&args.session_id, "reading lifecycle hooks", error))?
    else {
        return Ok(None);
    };
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
    if outcome.allow_prompt {
        Ok(None)
    } else {
        Ok(Some(
            outcome
                .block_reason
                .unwrap_or_else(|| "Prompt blocked by lifecycle hook".to_string()),
        ))
    }
}

async fn initialize_prompt_execution(input: PromptInput) -> Result<PromptExecution, SdkError> {
    let provider = create_prompt_provider_context(&input)?;
    let tool_state = initialize_prompt_tool_state(&input, &provider).await?;
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

struct PromptProviderContext {
    provider: Box<dyn LLMProvider>,
    runtime: ProviderRequestRuntime,
    model: String,
    reasoning_effort: Option<ReasoningEffortLevel>,
    supports_streaming: bool,
    supports_tools: bool,
}

fn create_prompt_provider_context(input: &PromptInput) -> Result<PromptProviderContext, SdkError> {
    let PromptInput { agent, args, session, .. } = input;
    let (provider_name, model, configured_reasoning_effort) = {
        let data = session.data.lock().map_err(|_err| SdkError::internal_error())?;
        (data.provider.clone(), data.model.clone(), data.reasoning_effort)
    };
    let runtime = agent.provider_runtime.for_provider(&provider_name);
    let mut timeouts = agent.provider_timeouts.clone();
    runtime.apply_http_timeouts(&mut timeouts);
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

async fn initialize_prompt_tool_state(
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
    if maybe_compact_prompt(agent, session, provider.provider.as_ref(), &provider.runtime, &provider.model, &tool_state)
        .await?
    {
        tool_state.messages = agent
            .resolved_messages(session)
            .map_err(|error| session_access_error(&args.session_id, "resolving compacted session messages", error))?;
    }
    Ok(tool_state)
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

pub(super) async fn run_prompt(agent: Arc<ZedAgent>, args: PromptRequest) -> Result<PromptResponse, SdkError> {
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
            streaming::run_streaming_provider_turn(&mut self).await?
        } else {
            buffered::run_buffered_provider_turn(&mut self).await?
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

fn resolve_api_key_for_provider(agent: &ZedAgent, provider: &str) -> String {
    if provider.eq_ignore_ascii_case(&agent.config.provider) && !agent.config.api_key.is_empty() {
        return agent.config.api_key.clone();
    }

    get_api_key_with_mode(provider, &ApiKeySources::default(), agent.credential_storage_mode).unwrap_or_default()
}

/// Streaming is safe unless a Stop hook needs to inspect the complete draft.
/// Other lifecycle hooks do not affect whether ACP can receive streamed text.
pub(super) fn should_allow_streaming(supports_streaming: bool, _tools_allowed: bool, has_stop_hooks: bool) -> bool {
    supports_streaming && !has_stop_hooks
}

pub(super) fn should_emit_buffered_final_chunk(allow_streaming: bool) -> bool {
    !allow_streaming
}

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
