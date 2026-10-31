//! Executes tool rounds requested by a provider response.
use super::{persist_session_checkpoint, update_transcript_path_best_effort};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::agent::ZedAgent;
use crate::zed::agent::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use crate::zed::types::SessionHandle;
use tracing::{info, warn};
use vtcode_core::llm::provider::{Message, ToolCall as LLMToolCall};

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

/// Whether a buffered turn still needs to relay assistant updates.
///
/// Streamed turns have already sent each chunk, and a cancelled turn must not
/// send updates the client can no longer render as live output.
fn should_relay_assistant_updates(streamed: bool, session: &SessionHandle) -> bool {
    !streamed && !session.cancellation.is_cancelled()
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
        if should_relay_assistant_updates(streamed, session) {
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

    if should_relay_assistant_updates(streamed, session) {
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

pub(in crate::zed::agent::handlers) fn assistant_tool_call_message(
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
        warn!(%error, boundary, "Failed to persist ACP session checkpoint");
        return Err(
            SdkError::internal_error().data(format!("Failed to persist ACP session checkpoint at {boundary}: {error}"))
        );
    }
    update_transcript_path_best_effort(session, boundary).await;
    Ok(())
}
