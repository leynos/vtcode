//! Collects provider stream events and records their user-visible outcomes.
use super::*;

pub(super) async fn collect_stream_events(
    stream: &mut LLMStream,
    request: StreamCollectionRequest<'_>,
) -> StreamAttemptOutcome {
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
    request
        .turn
        .agent
        .publish_lody_usage(&request.turn.args.session_id, &request.turn.session_model, &response);
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

pub(super) fn classify_stream_failure(request: StreamFailureRequest<'_>) -> StreamAttemptOutcome {
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
