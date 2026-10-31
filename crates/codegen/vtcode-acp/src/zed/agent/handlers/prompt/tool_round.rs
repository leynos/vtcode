//! Executes tool rounds requested by a provider response.
use super::{
    assistant_tool_call_message, persist_session_checkpoint, require_session_checkpoint, tool_execution_failed_error,
};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::agent::ZedAgent;
use crate::zed::agent::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use crate::zed::types::SessionHandle;
use tracing::{info, warn};
use vtcode_core::llm::provider::ToolCall as LLMToolCall;

pub(in crate::zed::agent::handlers) struct ToolRoundRequest<'a> {
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

pub(in crate::zed::agent::handlers) enum ToolRoundOutcome {
    Continue,
    Cancelled,
    LimitReached(String),
}

pub(in crate::zed::agent::handlers) async fn execute_tool_round(
    request: ToolRoundRequest<'_>,
) -> Result<ToolRoundOutcome, SdkError> {
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
        if !streamed && !session.cancellation.is_cancelled() {
            agent
                .send_assistant_updates(
                    session_id,
                    tool_loop_count.saturating_add(1),
                    assistant_message,
                    assistant_reasoning,
                    tool_calls.len(),
                )
                .await;
        }
        return Ok(ToolRoundOutcome::LimitReached(agent.tool_loop_limit_message()));
    }
    *tool_loop_count = tool_loop_count.saturating_add(1);

    let assistant_tool_message = assistant_tool_call_message(
        assistant_message.to_string(),
        tool_calls.to_vec(),
        assistant_reasoning,
        reasoning_details,
    );
    let thread = session.data.lock().map_err(|_err| SdkError::internal_error())?.thread.clone();
    drop(stage_thread_tool_calls(&thread, assistant_tool_message));
    let tool_call_count = tool_calls.len();
    info!(session_id = %session_id, tool_call_count, tool_loop_count = *tool_loop_count,
          tool_recovery_outcome = "placeholders_staged", "ACP tool recovery placeholders staged");
    require_session_checkpoint(agent, session, "assistant_tool_calls_write_ahead").await?;

    if !streamed && !session.cancellation.is_cancelled() {
        agent
            .send_assistant_updates(
                session_id,
                *tool_loop_count,
                assistant_message,
                assistant_reasoning,
                tool_call_count,
            )
            .await;
    }

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
