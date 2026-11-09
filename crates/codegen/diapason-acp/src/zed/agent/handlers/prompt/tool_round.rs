//! Executes one assistant tool-call round and records every result.
//!
//! The assistant message is appended before the tool calls run so an
//! interruption leaves a recoverable, unambiguous history for `tool_recovery`
//! to repair on the next session attach.

use super::*;
use super::{
    assistant_tool_call_message, persist_session_checkpoint, persist_terminal_tool_results, require_session_checkpoint,
};
use crate::zed::agent::tool_recovery::{replace_thread_tool_results, stage_thread_tool_calls};
use diapason_core::metrics::ToolRecoveryEvent;
use std::collections::HashSet;

/// Measures elapsed time for one tool round.
///
/// Production uses [`SystemToolRoundTimer`]; tests substitute a timer that
/// returns fixed values so the `elapsed_ms` span field is deterministic.
pub(in crate::zed::agent::handlers) trait ToolRoundTimer: Send + Sync {
    /// Marks the start of a measured tool round.
    fn start(&self) -> std::time::Instant;

    /// Returns elapsed milliseconds for a round started at `started`.
    fn elapsed_ms(&self, started: std::time::Instant) -> u64;
}

/// Production tool-round timer backed by the system clock.
pub(in crate::zed::agent::handlers) struct SystemToolRoundTimer;

impl ToolRoundTimer for SystemToolRoundTimer {
    fn start(&self) -> std::time::Instant {
        std::time::Instant::now()
    }

    fn elapsed_ms(&self, started: std::time::Instant) -> u64 {
        elapsed_ms(started)
    }
}

pub(in crate::zed::agent::handlers) struct ToolRoundRequest<'a> {
    pub(in crate::zed::agent::handlers) agent: &'a ZedAgent,
    pub(in crate::zed::agent::handlers) session: &'a SessionHandle,
    pub(in crate::zed::agent::handlers) session_id: &'a acp::SessionId,
    pub(in crate::zed::agent::handlers) tool_calls: &'a [LLMToolCall],
    pub(in crate::zed::agent::handlers) assistant_message: &'a str,
    pub(in crate::zed::agent::handlers) assistant_reasoning: &'a str,
    pub(in crate::zed::agent::handlers) reasoning_details: Option<Vec<String>>,
    pub(in crate::zed::agent::handlers) tool_loop_count: &'a mut usize,
    pub(in crate::zed::agent::handlers) streamed: bool,
    pub(in crate::zed::agent::handlers) timer: &'a dyn ToolRoundTimer,
}

pub(in crate::zed::agent::handlers) enum ToolRoundOutcome {
    Continue,
    Cancelled,
    LimitReached(String),
}

/// Rejects a provider batch whose tool-call IDs are not unique.
///
/// Duplicate IDs make placeholder replacement ambiguous: the second result
/// overwrites the first, leaving one placeholder unresolved, and archive repair
/// then refuses the batch. Rejecting before staging keeps history consistent.
pub(in crate::zed::agent::handlers) fn validate_tool_call_ids(tool_calls: &[LLMToolCall]) -> Result<(), SdkError> {
    let mut call_ids = HashSet::with_capacity(tool_calls.len());
    if tool_calls.iter().any(|call| !call_ids.insert(call.id.as_str())) {
        return Err(SdkError::internal_error().data("Provider returned duplicate tool-call IDs"));
    }
    Ok(())
}

/// Executes one assistant tool-call round and records every result.
///
/// The assistant message is appended before the tool calls run so an
/// interruption leaves a recoverable, unambiguous history for
/// `tool_recovery` to repair on the next session attach.
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
        timer,
    } = request;

    if agent.tool_loop_limit_reached(*tool_loop_count) {
        return Ok(ToolRoundOutcome::LimitReached(agent.tool_loop_limit_message()));
    }
    if let Err(error) = validate_tool_call_ids(tool_calls) {
        agent
            .recovery_metrics()
            .record_tool_recovery(ToolRecoveryEvent::DuplicateToolCallBatch);
        return Err(error);
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
    agent
        .recovery_metrics()
        .record_tool_recovery_events(ToolRecoveryEvent::StagedCall, tool_call_count);
    info!(session_id = %session_id, tool_call_count, tool_loop_count = *tool_loop_count,
          tool_recovery_outcome = "placeholders_staged", "ACP tool recovery placeholders staged");
    require_session_checkpoint(agent, session, "assistant_tool_calls_write_ahead").await?;

    let execution_span = tracing::info_span!(
        "acp_tool_execution",
        operation = "execute_tool_round",
        tool_call_count,
        tool_loop_count = *tool_loop_count,
        outcome = tracing::field::Empty,
        finalized_tool_count = tracing::field::Empty,
        elapsed_ms = tracing::field::Empty,
    );
    let started = timer.start();
    async {
        let tool_results = match agent.execute_tool_calls(session, session_id, tool_calls).await {
            Ok(results) => results,
            Err(error) => {
                let _ = execution_span.record("outcome", "execution_interrupted");
                let _ = execution_span.record("elapsed_ms", timer.elapsed_ms(started));
                agent
                    .recovery_metrics()
                    .record_tool_recovery_events(ToolRecoveryEvent::InterruptedCall, tool_call_count);
                // The raw error is not logged: its message can embed client
                // detail. A fixed category and the retry contract are enough
                // to diagnose the interruption.
                warn!(
                    session_id = %session_id,
                    tool_call_count,
                    error_category = execution_error_category(&error),
                    retryable = false,
                    tool_recovery_outcome = "execution_interrupted",
                    "Tool execution failed"
                );
                persist_session_checkpoint(agent, session, "interrupted_tool_results").await;
                return Err(error);
            }
        };

        let finalized_tool_count = replace_thread_tool_results(&thread, &tool_results);
        agent
            .recovery_metrics()
            .record_tool_recovery_events(ToolRecoveryEvent::ReplacedCall, finalized_tool_count);
        let _ = execution_span.record("outcome", "results_replaced");
        let _ = execution_span.record("finalized_tool_count", finalized_tool_count);
        let _ = execution_span.record("elapsed_ms", timer.elapsed_ms(started));
        info!(session_id = %session_id, tool_call_count, tool_loop_count = *tool_loop_count,
              finalized_tool_count, tool_recovery_outcome = "results_replaced", "ACP tool recovery results replaced");
        // The placeholders are now terminal results, so the batch is resolved
        // once this checkpoint proves the archive still carries it.
        persist_terminal_tool_results(agent, session, "tool_results").await;

        Ok(if session.cancellation.is_cancelled() {
            ToolRoundOutcome::Cancelled
        } else {
            ToolRoundOutcome::Continue
        })
    }
    .instrument(execution_span.clone())
    .await
}

/// Bounded category for an ACP tool-execution failure.
///
/// The category is a fixed variant so it is safe to log and to use as a span
/// field; the raw error message is not.
fn execution_error_category(error: &SdkError) -> &'static str {
    match error.code {
        acp::ErrorCode::ParseError => "parse_error",
        acp::ErrorCode::InvalidRequest => "invalid_request",
        acp::ErrorCode::MethodNotFound => "method_not_found",
        acp::ErrorCode::InvalidParams => "invalid_params",
        acp::ErrorCode::InternalError => "internal_error",
        acp::ErrorCode::RequestCancelled => "request_cancelled",
        acp::ErrorCode::AuthRequired => "auth_required",
        acp::ErrorCode::ResourceNotFound => "resource_not_found",
        _ => "other",
    }
}

/// Elapsed milliseconds, saturating rather than wrapping.
fn elapsed_ms(started: std::time::Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
