//! Checkpoints session state and commits non-cancelled provider failures.
use super::*;
use crate::zed::agent::log_suppressed_error;

#[tracing::instrument(skip_all, fields(boundary = boundary))]
pub(in crate::zed::agent::handlers) async fn persist_session_checkpoint(
    agent: &ZedAgent,
    session: &SessionHandle,
    boundary: &'static str,
) {
    if let Err(error) = agent.checkpoint_session(session).await {
        log_suppressed_error(&error, "session_checkpoint", "archive_write_failed");
        warn!(
            operation = "session_checkpoint",
            error_category = "archive_write_failed",
            boundary,
            "Failed to persist ACP session checkpoint"
        );
    }
    update_transcript_path_best_effort(session, boundary).await;
}

/// Refreshes the session transcript path, logging a failure without failing the turn.
async fn update_transcript_path_best_effort(session: &SessionHandle, boundary: &'static str) {
    if let Err(error) = session.update_transcript_path().await {
        log_suppressed_error(&error, "transcript_update", "transcript_write_failed");
        warn!(
            operation = "transcript_update",
            error_category = "transcript_write_failed",
            boundary,
            "Failed to update ACP session transcript path"
        );
    }
}

pub(in crate::zed::agent::handlers) async fn finish_failed_provider_turn(
    turn: &PromptExecution,
    error: SafeProviderProjection,
) -> Result<PromptResponse, SdkError> {
    let IncompleteProviderTurn { message, visible_update, response } =
        IncompleteProviderTurn::from_failure(&turn.assistant_message, &turn.assistant_reasoning, error);
    if let Err(error) = turn
        .agent
        .send_update(&turn.args.session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(visible_update)))
        .await
    {
        warn!(
            session_id = %turn.args.session_id,
            update_kind = "agent_message_chunk",
            error = %error,
            "Failed to send assistant update to ACP client"
        );
    }
    push_session_message(&turn.agent, &turn.session, &turn.args.session_id, message)?;
    persist_session_checkpoint(&turn.agent, &turn.session, "incomplete_provider_turn").await;
    warn!(
        error_category = error.category(),
        error_status = ?error.status(),
        partial_text_bytes = turn.assistant_message.len(),
        partial_reasoning_bytes = turn.assistant_reasoning.len(),
        "ACP provider failed to complete the turn"
    );
    Ok(response)
}

pub(in crate::zed::agent::handlers) async fn finish_failed_output(
    turn: &mut PromptExecution,
    output: ProviderOutput,
    error: SafeProviderProjection,
) -> Result<ProviderTurnOutcome, SdkError> {
    output.store_in(turn);
    finish_failed_provider_turn(turn, error)
        .await
        .map(ProviderTurnOutcome::Response)
}

/// Persists the strict write-ahead checkpoint required before tool execution.
///
/// Ordinary transcript checkpoints remain best-effort through
/// [`persist_session_checkpoint`].
pub(in crate::zed::agent::handlers) async fn require_session_checkpoint(
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
