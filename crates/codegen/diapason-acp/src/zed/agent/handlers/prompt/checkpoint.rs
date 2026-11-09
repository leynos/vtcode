//! Session checkpoint helpers for ACP prompt handling.
//!
//! Ordinary checkpoints log a failure without failing the prompt, while the
//! tool-round write-ahead checkpoint fails the turn when it was not made
//! durable. Both update the transcript path afterwards so a resumed session
//! can find the archive that owns its history.

use super::*;
use crate::zed::agent::log_suppressed_error;
use crate::zed::agent::session_state::checkpoint_failure_label;
use diapason_core::metrics::ToolRecoveryEvent;

/// Persists the session checkpoint and transcript path, logging failures
/// without failing the prompt.
#[tracing::instrument(skip_all, fields(boundary = boundary))]
pub(in crate::zed::agent::handlers) async fn persist_session_checkpoint(
    agent: &ZedAgent,
    session: &SessionHandle,
    boundary: &'static str,
) {
    if let Err(error) = agent.checkpoint_session(session).await {
        record_checkpoint_failure(agent, &error, boundary);
    }
    update_transcript_path(session, boundary).await;
}

/// Persists terminal tool results and releases write-ahead protection.
///
/// The caller has replaced every staged placeholder with a terminal result, so
/// the batch no longer represents an uncertain effect once the archive carries
/// it. [`ZedAgent::checkpoint_terminal_tool_results`] releases the protection
/// only after a write that retains the whole batch, so a skipped or failed
/// write keeps guarding the archive that still holds it.
#[tracing::instrument(skip_all, fields(boundary = boundary))]
pub(in crate::zed::agent::handlers) async fn persist_terminal_tool_results(
    agent: &ZedAgent,
    session: &SessionHandle,
    boundary: &'static str,
) {
    if let Err(error) = agent.checkpoint_terminal_tool_results(session).await {
        record_checkpoint_failure(agent, &error, boundary);
    }
    update_transcript_path(session, boundary).await;
}

/// Records a checkpoint failure without failing the prompt.
fn record_checkpoint_failure(agent: &ZedAgent, error: &anyhow::Error, boundary: &'static str) {
    agent
        .recovery_metrics()
        .record_tool_recovery(ToolRecoveryEvent::CheckpointFailure);
    // The raw chain is never logged, even at debug level: it can name archive
    // paths or storage detail. A bounded category and the chain depth are
    // enough to diagnose the failure.
    warn!(
        operation = "session_checkpoint",
        error_category = checkpoint_failure_label(error),
        error_chain_depth = error.chain().count(),
        boundary,
        "Failed to persist ACP session checkpoint"
    );
}

/// Updates the transcript path, logging a failure without failing the prompt.
async fn update_transcript_path(session: &SessionHandle, boundary: &'static str) {
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

/// Persists the strict write-ahead checkpoint required before tool execution.
///
/// Ordinary transcript checkpoints remain best-effort through
/// [`persist_session_checkpoint`]. This path fails when the snapshot was not
/// durably persisted, including when history persistence is disabled, so a
/// tool never runs without a recoverable record of its staged call.
pub(super) async fn require_session_checkpoint(
    agent: &ZedAgent,
    session: &SessionHandle,
    boundary: &'static str,
) -> Result<(), SdkError> {
    if let Err(error) = agent.checkpoint_session_required(session).await {
        agent
            .recovery_metrics()
            .record_tool_recovery(ToolRecoveryEvent::CheckpointFailure);
        // The raw error never reaches the client or the log: it can name archive
        // paths or storage detail. Report one bounded category instead so log
        // queries and client errors distinguish a write failure, an unreadable
        // read-back and a compacted payload that dropped staged evidence.
        let error_category = checkpoint_failure_label(&error);
        warn!(
            operation = "session_checkpoint",
            error_category,
            error_chain_depth = error.chain().count(),
            boundary,
            "Failed to persist ACP session checkpoint"
        );
        return Err(SdkError::internal_error()
            .data(format!("Failed to persist ACP session checkpoint at {boundary}: {error_category}")));
    }
    update_transcript_path(session, boundary).await;
    Ok(())
}
