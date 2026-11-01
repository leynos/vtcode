//! Collects provider stream events and projects text, tool, timeout, and retry outcomes into ACP turns.
use super::super::provider::*;

mod event_collection;
use super::buffered::refresh_resolved_tool_state;
use super::*;
use event_collection::classify_stream_failure;

enum StreamAttemptOutcome {
    Completed(Box<LLMResponse>),
    Retry {
        delay: Duration,
        error: LLMError,
        observed_at: SystemTime,
    },
    Failed {
        error: LLMError,
        observed_at: SystemTime,
    },
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
    backoff: &'a mut RetryBackoff,
    retry_observed_at: &'a RetryObservationClock<'a>,
}

struct StreamFailureRequest<'a> {
    turn: &'a PromptExecution,
    telemetry: &'a mut GenerationTelemetry,
    output: &'a ProviderOutput,
    attempt_index: u32,
    error: LLMError,
    backoff: &'a mut RetryBackoff,
    retry_observed_at: &'a RetryObservationClock<'a>,
}

struct StreamAttemptRequest<'a> {
    turn: &'a PromptExecution,
    request: &'a LLMRequest,
    output: &'a mut ProviderOutput,
    attempt_index: u32,
    backoff: &'a mut RetryBackoff,
    retry_observed_at: &'a RetryObservationClock<'a>,
}

struct StreamHandleRequest<'a> {
    turn: &'a mut PromptExecution,
    output: &'a mut ProviderOutput,
    attempt: StreamAttemptOutcome,
    attempt_index: u32,
    tool_loop_count: &'a mut usize,
}

struct StreamStartRequest<'a> {
    turn: &'a PromptExecution,
    request: &'a LLMRequest,
    deadline_policy: ProviderDeadlinePolicy,
    deadlines: &'a mut StreamDeadlineTracker,
    span: &'a Span,
    started_at: Instant,
}

enum StreamStartError {
    Cancelled,
    Failed(LLMError),
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

pub(super) async fn run_streaming_provider_turn(
    turn: &mut PromptExecution,
    retry_observed_at: &RetryObservationClock<'_>,
) -> Result<ProviderTurnOutcome, SdkError> {
    let mut output = ProviderOutput::take_from(turn);
    let mut tool_loop_count = 0usize;
    let mut attempt_index = 0u32;
    let mut backoff = RetryBackoff::new();

    loop {
        if turn.session.cancellation.is_cancelled() {
            output.store_in(turn);
            return Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled));
        }
        let request = streaming_request(turn);
        let attempt = collect_streaming_attempt(StreamAttemptRequest {
            turn,
            request: &request,
            output: &mut output,
            attempt_index,
            backoff: &mut backoff,
            retry_observed_at,
        })
        .await?;
        match handle_streaming_attempt(StreamHandleRequest {
            turn,
            output: &mut output,
            attempt,
            attempt_index,
            tool_loop_count: &mut tool_loop_count,
        })
        .await?
        {
            StreamingTurnOutcome::Retry => attempt_index = attempt_index.saturating_add(1),
            StreamingTurnOutcome::ContinueAfterTools => {
                attempt_index = 0;
                backoff.reset();
            }
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

async fn handle_streaming_attempt(request: StreamHandleRequest<'_>) -> Result<StreamingTurnOutcome, SdkError> {
    let StreamHandleRequest {
        turn,
        output,
        attempt,
        attempt_index,
        tool_loop_count,
    } = request;
    match attempt {
        StreamAttemptOutcome::Completed(response) => {
            match process_stream_response(turn, output, *response, tool_loop_count).await? {
                StreamResponseOutcome::Continue => Ok(StreamingTurnOutcome::ContinueAfterTools),
                StreamResponseOutcome::StopReason(reason) => {
                    Ok(StreamingTurnOutcome::Finished(ProviderTurnOutcome::StopReason(reason)))
                }
            }
        }
        StreamAttemptOutcome::Retry { delay, error, observed_at } => {
            turn.agent
                .publish_rate_limit_notice_with_context(RateLimitNotice {
                    session_id: &turn.args.session_id,
                    provider: turn.provider_runtime.provider_name(),
                    error: &error,
                    retry_delay: Some(delay),
                    observed_at,
                    metric_sink: &turn.metric_sink,
                    provider_class: turn.provider_runtime.provider_class(),
                })
                .await;
            retry_stream_attempt(turn, output, delay, attempt_index).await
        }
        StreamAttemptOutcome::Failed { error, observed_at } => {
            turn.agent
                .publish_rate_limit_notice_with_context(RateLimitNotice {
                    session_id: &turn.args.session_id,
                    provider: turn.provider_runtime.provider_name(),
                    error: &error,
                    retry_delay: None,
                    observed_at,
                    metric_sink: &turn.metric_sink,
                    provider_class: turn.provider_runtime.provider_class(),
                })
                .await;
            let response = finish_failed_output(
                turn,
                std::mem::replace(output, ProviderOutput::empty()),
                SafeProviderProjection::from_llm_error(&error),
            )
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
        provider_class = turn.provider_runtime.provider_class().as_str(),
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
                finish_failed_output(turn, std::mem::replace(output, ProviderOutput::empty()), error).await?;
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

async fn start_provider_stream(request: StreamStartRequest<'_>) -> Result<LLMStream, StreamStartError> {
    let StreamStartRequest {
        turn,
        request,
        deadline_policy,
        deadlines,
        span,
        started_at,
    } = request;
    let next_deadline = deadlines.next();
    let stream_result = tokio::select! {
        () = turn.session.cancellation.cancelled() => {
            record_stream_attempt(span, started_at, "cancelled");
            return Err(StreamStartError::Cancelled);
        }
        phase = sleep_until_stream_deadline(next_deadline) => {
            Err(provider_timeout_error(
                turn.provider_runtime.provider_name(),
                phase,
                phase.timeout(deadline_policy),
            ))
        }
        result = turn.provider.stream(request.clone()).instrument(span.clone()) => result,
    };
    stream_result.map_err(StreamStartError::Failed)
}

/// Instrumentation span for one provider stream attempt.
fn stream_attempt_span(provider_runtime: &ProviderRequestRuntime) -> Span {
    tracing::info_span!(
        "acp_provider_stream_attempt",
        operation = "provider_stream_attempt",
        provider_class = provider_runtime.provider_class().as_str(),
        outcome = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    )
}

async fn collect_streaming_attempt(request: StreamAttemptRequest<'_>) -> Result<StreamAttemptOutcome, SdkError> {
    let StreamAttemptRequest {
        turn,
        request,
        output,
        attempt_index,
        backoff,
        retry_observed_at,
    } = request;
    let provider_runtime = &turn.provider_runtime;
    let deadline_policy = provider_runtime.deadline_policy();
    let permit = match provider_runtime.acquire(&turn.session.cancellation).await {
        Ok(permit) => permit,
        Err(ProviderAdmissionError::Cancelled) => return Ok(StreamAttemptOutcome::Cancelled),
        Err(error) => return Err(SdkError::internal_error().data(error.to_string())),
    };
    let mut telemetry = GenerationTelemetry::start(provider_runtime, turn.metric_sink.clone(), ProviderMode::Stream);
    telemetry.record_attempt(provider_runtime).await;
    let mut deadlines = StreamDeadlineTracker::new(deadline_policy, Instant::now());
    let started_at = Instant::now();
    let span = stream_attempt_span(provider_runtime);
    let mut stream = match start_provider_stream(StreamStartRequest {
        turn,
        request,
        deadline_policy,
        deadlines: &mut deadlines,
        span: &span,
        started_at,
    })
    .await
    {
        Ok(stream) => stream,
        Err(StreamStartError::Cancelled) => return Ok(StreamAttemptOutcome::Cancelled),
        Err(StreamStartError::Failed(error)) => {
            record_stream_attempt(&span, started_at, "failure");
            return Ok(classify_stream_failure(StreamFailureRequest {
                turn,
                telemetry: &mut telemetry,
                output,
                attempt_index,
                error,
                backoff,
                retry_observed_at,
            })
            .await);
        }
    };

    let outcome = event_collection::collect_stream_events(
        &mut stream,
        StreamCollectionRequest {
            turn,
            span: &span,
            started_at,
            attempt_index,
            deadlines: &mut deadlines,
            telemetry: &mut telemetry,
            output,
            backoff,
            retry_observed_at,
        },
    )
    .await;
    drop(stream);
    drop(permit);
    Ok(outcome)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::agent::handlers::tests::provider_runtime_fixtures::{TraceCapture, retry_runtime};
    use tracing_subscriber::Registry;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn stream_attempt_span_uses_bounded_provider_class() {
        let capture = TraceCapture::default();
        let subscriber = Registry::default().with(capture.clone());
        tracing::subscriber::with_default(subscriber, || {
            let _span = stream_attempt_span(&retry_runtime());
        });

        let captured = capture.0.lock().expect("trace capture lock").join("\n");
        let span_line = captured
            .lines()
            .find(|line| line.starts_with("span=acp_provider_stream_attempt"))
            .expect("the stream attempt span should be captured");
        assert!(span_line.contains("operation=\"provider_stream_attempt\""));
        assert!(span_line.contains("provider_class=\"custom\""));
        assert!(
            !span_line.contains("provider=\"retry-test\""),
            "stream attempt spans must not record the raw provider name"
        );
    }
}
