//! Handles complete provider responses, including reasoning updates and buffered tool calls.
pub(in crate::zed::agent::handlers) fn response_reasoning_update(response: &LLMResponse) -> Option<acp::SessionUpdate> {
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

use super::super::provider::*;
use super::*;

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

enum BufferedGenerationStep {
    Ready(Box<LLMResponse>),
    Finished(ProviderTurnOutcome),
}

enum BufferedTurnStep {
    Continue,
    Finished(ProviderTurnOutcome),
}

pub(super) async fn run_buffered_provider_turn(turn: &mut PromptExecution) -> Result<ProviderTurnOutcome, SdkError> {
    let mut output = ProviderOutput::take_from(turn);
    let mut tool_loop_count = 0usize;

    loop {
        match buffered_turn_step(turn, &mut output, &mut tool_loop_count).await? {
            BufferedTurnStep::Continue => {}
            BufferedTurnStep::Finished(outcome) => return Ok(outcome),
        }
    }
}

async fn buffered_turn_step(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    tool_loop_count: &mut usize,
) -> Result<BufferedTurnStep, SdkError> {
    let response = match next_buffered_generation(turn, output).await? {
        BufferedGenerationStep::Ready(response) => *response,
        BufferedGenerationStep::Finished(outcome) => return Ok(BufferedTurnStep::Finished(outcome)),
    };

    match execute_buffered_tool_calls(turn, &response, tool_loop_count).await? {
        BufferedToolOutcome::Continue => return Ok(BufferedTurnStep::Continue),
        BufferedToolOutcome::Finished(stop_reason, message) => {
            if let Some(message) = message {
                output.assistant_message = message;
            }
            store_buffered_output(turn, output);
            return Ok(BufferedTurnStep::Finished(ProviderTurnOutcome::StopReason(stop_reason)));
        }
        BufferedToolOutcome::NoToolCalls => {}
    }

    match process_buffered_response(turn, output, response).await? {
        BufferedResponseOutcome::Continue => Ok(BufferedTurnStep::Continue),
        BufferedResponseOutcome::Finished(stop_reason) => {
            store_buffered_output(turn, output);
            Ok(BufferedTurnStep::Finished(ProviderTurnOutcome::StopReason(stop_reason)))
        }
    }
}

async fn next_buffered_generation(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
) -> Result<BufferedGenerationStep, SdkError> {
    if turn.session.cancellation.is_cancelled() {
        store_buffered_output(turn, output);
        return Ok(BufferedGenerationStep::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)));
    }
    let response = match generate_buffered_response(turn).await? {
        BufferedGenerationOutcome::Response(response) => *response,
        BufferedGenerationOutcome::Cancelled => {
            store_buffered_output(turn, output);
            return Ok(BufferedGenerationStep::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)));
        }
        BufferedGenerationOutcome::Failed(error) => {
            let pending_output = std::mem::replace(output, ProviderOutput::empty());
            return finish_failed_output(turn, pending_output, &error)
                .await
                .map(BufferedGenerationStep::Finished);
        }
    };
    turn.agent.publish_lody_usage(
        &turn.args.session_id,
        turn.provider_runtime.provider_name(),
        &turn.session_model,
        &response,
    );
    if turn.session.cancellation.is_cancelled() {
        store_buffered_output(turn, output);
        return Ok(BufferedGenerationStep::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)));
    }
    emit_response_reasoning(&turn.agent, &turn.args.session_id, &response).await;
    if turn.session.cancellation.is_cancelled() {
        store_buffered_output(turn, output);
        return Ok(BufferedGenerationStep::Finished(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled)));
    }
    Ok(BufferedGenerationStep::Ready(Box::new(response)))
}

fn store_buffered_output(turn: &mut PromptExecution, output: &mut ProviderOutput) {
    std::mem::replace(output, ProviderOutput::empty()).store_in(turn);
}

async fn generate_buffered_response(turn: &mut PromptExecution) -> Result<BufferedGenerationOutcome, SdkError> {
    let request = prepare_buffered_request(turn).await?;
    match generate_with_retry(
        turn.provider.as_ref(),
        request,
        &turn.provider_runtime,
        &turn.session.cancellation,
        Some((&turn.agent, &turn.args.session_id)),
    )
    .await
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

pub(super) fn refresh_resolved_tool_state(turn: &mut PromptExecution) -> Result<(), SdkError> {
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
