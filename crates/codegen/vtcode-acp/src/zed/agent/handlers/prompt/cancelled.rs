//! Commits cancelled provider turns without discarding text the client saw.
use super::*;

/// Flushes any buffered assistant text that never reached the client, then
/// reports cancellation. Used when a provider failure follows `session/cancel`:
/// ACP requires `Cancelled`, not an `EndTurn` failure notice.
///
/// A flushed draft becomes client-visible, so it is also persisted: durable
/// history must match what the user saw. Text that was already streamed keeps
/// the cancellation path's existing history behaviour.
pub(in crate::zed::agent::handlers) async fn finish_cancelled_output(
    turn: &mut PromptExecution,
    mut output: ProviderOutput,
) -> Result<ProviderTurnOutcome, SdkError> {
    let flushed = output.take_unemitted_assistant_text().is_some();
    if flushed {
        let text = output.assistant_message.clone();
        drop(
            turn.agent
                .send_update(&turn.args.session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(text)))
                .await,
        );
    }
    let visible_message = flushed.then(|| {
        let mut message = Message::assistant(output.assistant_message.clone());
        if !output.assistant_reasoning.is_empty() {
            message.reasoning = Some(output.assistant_reasoning.clone());
        }
        message
    });
    output.store_in(turn);
    if let Some(message) = visible_message {
        // Cancellation must keep `StopReason::Cancelled` even if persisting the
        // visible draft fails, so this is best-effort like other checkpoints.
        match push_session_message(&turn.agent, &turn.session, &turn.args.session_id, message) {
            Ok(()) => persist_session_checkpoint(&turn.agent, &turn.session, "assistant_response_cancelled").await,
            Err(error) => warn!(%error, "Failed to persist the visible cancelled ACP draft"),
        }
    }
    Ok(ProviderTurnOutcome::StopReason(acp::StopReason::Cancelled))
}

/// Commits a provider failure, preferring `Cancelled` when the turn was
/// cancelled while the error was in flight. Centralizes the rule shared by the
/// buffered and streaming failure paths.
pub(in crate::zed::agent::handlers) async fn finish_failed_or_cancelled_output(
    turn: &mut PromptExecution,
    output: &mut ProviderOutput,
    error: SafeProviderProjection,
) -> Result<ProviderTurnOutcome, SdkError> {
    let pending = std::mem::replace(output, ProviderOutput::empty());
    if turn.session.cancellation.is_cancelled() {
        return finish_cancelled_output(turn, pending).await;
    }
    finish_failed_output(turn, pending, error).await
}
