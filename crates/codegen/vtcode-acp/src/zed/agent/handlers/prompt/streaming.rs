//! Collects provider stream events and projects text, tool, timeout, and retry outcomes into ACP turns.
use super::super::provider::*;
use super::buffered::refresh_resolved_tool_state;
use super::*;

pub(super) async fn run_streaming_provider_turn(turn: &mut PromptExecution) -> Result<ProviderTurnOutcome, SdkError> {
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
                return Ok(preserve_streaming_output(turn, output, outcome));
            }
        }
    }
}

fn preserve_streaming_output(
    turn: &mut PromptExecution,
    output: ProviderOutput,
    outcome: ProviderTurnOutcome,
) -> ProviderTurnOutcome {
    if matches!(&outcome, ProviderTurnOutcome::StopReason(_)) {
        output.store_in(turn);
    }
    outcome
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
    let mut request = request;
    loop {
        match poll_next_stream_event(stream, &request).await {
            StreamPollOutcome::Cancelled => {
                record_stream_attempt(request.span, request.started_at, "cancelled");
                return StreamAttemptOutcome::Cancelled;
            }
            StreamPollOutcome::Deadline(phase) => {
                let policy = request.turn.provider_runtime.deadline_policy();
                let error =
                    provider_timeout_error(request.turn.provider_runtime.provider_name(), phase, phase.timeout(policy));
                return classify_stream_poll_failure(&mut request, error);
            }
            StreamPollOutcome::Item(None) => {
                let error = LLMError::Network {
                    message: "provider stream ended before a completion event".to_string(),
                    metadata: None,
                };
                return classify_stream_poll_failure(&mut request, error);
            }
            StreamPollOutcome::Item(Some(Err(error))) => return classify_stream_poll_failure(&mut request, error),
            StreamPollOutcome::Item(Some(Ok(event))) => {
                if let Some(outcome) = process_stream_event(event, &mut request).await {
                    return outcome;
                }
            }
        }
    }
}

enum StreamPollOutcome {
    Cancelled,
    Deadline(StreamTimeoutPhase),
    Item(Option<Result<LLMStreamEvent, LLMError>>),
}

async fn poll_next_stream_event(stream: &mut LLMStream, request: &StreamCollectionRequest<'_>) -> StreamPollOutcome {
    let next_deadline = request.deadlines.next();
    tokio::select! {
        () = request.turn.session.cancellation.cancelled() => StreamPollOutcome::Cancelled,
        phase = sleep_until_stream_deadline(next_deadline) => StreamPollOutcome::Deadline(phase),
        event = poll_provider_stream(stream, request.span.clone()) => StreamPollOutcome::Item(event),
    }
}

fn classify_stream_poll_failure(request: &mut StreamCollectionRequest<'_>, error: LLMError) -> StreamAttemptOutcome {
    record_stream_attempt(request.span, request.started_at, "failure");
    classify_stream_failure(StreamFailureRequest {
        turn: request.turn,
        telemetry: &mut *request.telemetry,
        output: request.output,
        attempt_index: request.attempt_index,
        error,
    })
}

async fn process_stream_event(
    event: LLMStreamEvent,
    request: &mut StreamCollectionRequest<'_>,
) -> Option<StreamAttemptOutcome> {
    match event {
        LLMStreamEvent::Token { delta } => {
            append_stream_delta(StreamDeltaRequest {
                turn: request.turn,
                output: &mut *request.output,
                deadlines: &mut *request.deadlines,
                telemetry: &mut *request.telemetry,
                attempt_index: request.attempt_index,
                delta,
                reasoning: false,
            })
            .await;
            None
        }
        LLMStreamEvent::Reasoning { delta } => {
            append_stream_delta(StreamDeltaRequest {
                turn: request.turn,
                output: &mut *request.output,
                deadlines: &mut *request.deadlines,
                telemetry: &mut *request.telemetry,
                attempt_index: request.attempt_index,
                delta,
                reasoning: true,
            })
            .await;
            None
        }
        LLMStreamEvent::ReasoningStage { .. } | LLMStreamEvent::ReasoningSignature { .. } => None,
        LLMStreamEvent::Completed { response } => Some(complete_stream_response(request, *response).await),
    }
}

async fn complete_stream_response(
    request: &mut StreamCollectionRequest<'_>,
    response: LLMResponse,
) -> StreamAttemptOutcome {
    record_stream_attempt(request.span, request.started_at, "completion");
    if request.telemetry.first_output_at.is_none() {
        observe_response_output(request, &response);
    }
    request
        .telemetry
        .complete(&request.turn.provider_runtime, &response, request.attempt_index, false);
    emit_response_fallback(request.turn, request.output, &response).await;
    StreamAttemptOutcome::Completed(Box::new(response))
}

fn observe_response_output(request: &mut StreamCollectionRequest<'_>, response: &LLMResponse) {
    if let Some(content) = response.content.as_deref() {
        request
            .telemetry
            .observe_output(&request.turn.provider_runtime, content, request.attempt_index);
    }
    if let Some(reasoning) = response.reasoning.as_deref() {
        request
            .telemetry
            .observe_output(&request.turn.provider_runtime, reasoning, request.attempt_index);
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
