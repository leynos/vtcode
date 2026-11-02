//! Commits cancelled provider turns without discarding text the client saw.
use super::*;

/// Publishes a buffered draft that never reached the client, returning whether
/// a chunk was sent. Text that was already streamed is left alone so the
/// client sees it exactly once.
async fn publish_cancelled_draft(agent: &ZedAgent, session_id: &acp::SessionId, output: &mut ProviderOutput) -> bool {
    let Some(text) = output.take_unemitted_assistant_text() else {
        return false;
    };
    drop(
        agent
            .send_update(session_id, acp::SessionUpdate::AgentMessageChunk(text_chunk(text)))
            .await,
    );
    true
}

/// Builds the durable message for assistant text the client has seen.
///
/// `flushed` reports a draft published by [`publish_cancelled_draft`]; text
/// streamed earlier in the turn is visible through `assistant_emitted`.
fn visible_cancelled_message(output: &ProviderOutput, flushed: bool) -> Option<Message> {
    if !flushed && !output.assistant_emitted {
        return None;
    }
    if output.assistant_message.is_empty() {
        return None;
    }
    let mut message = Message::assistant(output.assistant_message.clone());
    if !output.assistant_reasoning.is_empty() {
        message.reasoning = Some(output.assistant_reasoning.clone());
    }
    Some(message)
}

/// Appends a visible draft to durable history and checkpoints it.
///
/// Cancellation must keep `StopReason::Cancelled` even if persisting fails, so
/// this is best-effort like other transcript checkpoints.
async fn persist_cancelled_draft(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    message: Message,
) {
    match push_session_message(agent, session, session_id, message) {
        Ok(()) => persist_session_checkpoint(agent, session, "assistant_response_cancelled").await,
        Err(error) => warn!(%error, "Failed to persist the visible cancelled ACP draft"),
    }
}

/// Publishes and persists whatever part of a cancelled turn the client saw.
///
/// Kept separate from [`finish_cancelled_output`] so the delivery and durable
/// history steps can be exercised without a full prompt turn.
pub(in crate::zed::agent::handlers) async fn commit_cancelled_draft(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    output: &mut ProviderOutput,
) {
    let flushed = publish_cancelled_draft(agent, session_id, output).await;
    if let Some(message) = visible_cancelled_message(output, flushed) {
        persist_cancelled_draft(agent, session, session_id, message).await;
    }
}

/// Reports cancellation after committing the draft the client saw.
///
/// Used when a provider failure follows `session/cancel`: ACP requires
/// `Cancelled`, not an `EndTurn` failure notice. A buffered draft is flushed
/// first, then both flushed and streamed drafts are persisted, so durable
/// history matches what the user saw.
pub(in crate::zed::agent::handlers) async fn finish_cancelled_output(
    turn: &mut PromptExecution,
    mut output: ProviderOutput,
) -> Result<ProviderTurnOutcome, SdkError> {
    commit_cancelled_draft(&turn.agent, &turn.session, &turn.args.session_id, &mut output).await;
    output.store_in(turn);
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
    // Tests arm this pause to order `session/cancel` ahead of the decision
    // below; the failure has already been classified at this point.
    #[cfg(test)]
    turn.agent.checkpoint_pause.wait_at("provider_failure_classified").await;
    let pending = std::mem::replace(output, ProviderOutput::empty());
    if turn.session.cancellation.is_cancelled() {
        return finish_cancelled_output(turn, pending).await;
    }
    finish_failed_output(turn, pending, error).await
}

/// Test seam: runs the cancellation commit for a synthetic draft.
#[cfg(test)]
pub(crate) async fn commit_cancelled_draft_for_test(
    agent: &ZedAgent,
    session: &SessionHandle,
    session_id: &acp::SessionId,
    message: &str,
    assistant_emitted: bool,
) {
    let mut output = ProviderOutput {
        assistant_message: message.to_string(),
        assistant_reasoning: String::new(),
        emitted_output: !message.is_empty(),
        assistant_emitted,
    };
    commit_cancelled_draft(agent, session, session_id, &mut output).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    use proptest::prelude::*;

    proptest! {
        /// Durable history records exactly the text the client saw: present iff
        /// a chunk was published or streamed, and never for empty text.
        #[test]
        fn visible_message_requires_client_visible_text(
            text in "[a-z ]{0,24}",
            reasoning in "[a-z ]{0,24}",
            flushed in any::<bool>(),
            assistant_emitted in any::<bool>(),
        ) {
            let output = ProviderOutput {
                assistant_message: text.clone(),
                assistant_reasoning: reasoning.clone(),
                emitted_output: !text.is_empty(),
                assistant_emitted,
            };

            let recorded = visible_cancelled_message(&output, flushed);

            prop_assert_eq!(
                recorded.is_some(),
                (flushed || assistant_emitted) && !text.is_empty(),
                "history must follow what the client saw"
            );
            if let Some(recorded) = recorded {
                let recorded_text = recorded.content.as_text();
                prop_assert_eq!(recorded_text.as_ref(), text.as_str());
                prop_assert_eq!(
                    recorded.reasoning.as_deref(),
                    (!reasoning.is_empty()).then_some(reasoning.as_str()),
                    "reasoning accompanies the message only when it is non-empty"
                );
            }
        }
    }
}
