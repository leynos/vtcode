//! Applies ACP turn lifecycle hooks and commits completed assistant responses.
use super::super::super::{warn_hook_block_reason, warn_hook_message};
use super::*;

impl PromptExecution {
    pub(super) async fn finish(self, stop_reason: acp::StopReason) -> Result<PromptResponse, SdkError> {
        if stop_reason == acp::StopReason::Cancelled {
            return Ok(PromptResponse::new(stop_reason));
        }
        if !self.stop_hook_checked
            && let Some(reason) = self
                .agent
                .run_stop_hook(&self.session, &self.assistant_message, self.stop_hook_active)
                .await?
        {
            warn_hook_block_reason("Stop", &reason);
            push_session_message(&self.agent, &self.session, &self.args.session_id, Message::system(reason))?;
            persist_session_checkpoint(&self.agent, &self.session, "stop_hook_blocked").await;
            return Ok(PromptResponse::new(acp::StopReason::Refusal));
        }
        self.finish_assistant_turn(stop_reason).await
    }

    /// Relays a buffered final draft the client has not seen, commits the
    /// assistant message to durable history, and enforces the content-or-error
    /// rule for the turn.
    async fn finish_assistant_turn(mut self, stop_reason: acp::StopReason) -> Result<PromptResponse, SdkError> {
        let has_assistant_output = !self.assistant_message.is_empty() || !self.assistant_reasoning.is_empty();
        let mut assistant_emitted = self.assistant_emitted;
        if has_assistant_output && !assistant_emitted && should_emit_buffered_final_chunk(self.allow_streaming) {
            for failure in self
                .agent
                .send_assistant_updates(
                    &self.args.session_id,
                    self.provider_iteration,
                    &self.assistant_message,
                    &self.assistant_reasoning,
                    0,
                )
                .await
            {
                warn!(
                    session_id = %self.args.session_id,
                    update_kind = failure.kind.as_str(),
                    error = %failure.error,
                    "Failed to send assistant update to ACP client"
                );
            }
            // Thought chunks do not satisfy the content rule; only a delivered
            // commentary chunk makes the turn visible to the client.
            assistant_emitted |= !self.assistant_message.is_empty();
        }
        if self.assistant_message.is_empty() {
            require_assistant_content(stop_reason, assistant_emitted)?;
            return Ok(PromptResponse::new(stop_reason));
        }
        let mut completed_message = Message::assistant(self.assistant_message);
        if !self.assistant_reasoning.is_empty() {
            completed_message.reasoning = Some(self.assistant_reasoning);
        }
        push_session_message(&self.agent, &self.session, &self.args.session_id, completed_message)?;
        persist_session_checkpoint(&self.agent, &self.session, "assistant_response").await;
        require_assistant_content(stop_reason, assistant_emitted)?;
        Ok(PromptResponse::new(stop_reason))
    }
}

/// A non-cancelled `EndTurn` without any assistant chunk would acknowledge a
/// prompt with no response; fail explicitly instead.
fn require_assistant_content(stop_reason: acp::StopReason, assistant_emitted: bool) -> Result<(), SdkError> {
    if stop_reason == acp::StopReason::EndTurn && !assistant_emitted {
        return Err(empty_response_error());
    }
    Ok(())
}

impl ZedAgent {
    pub(super) async fn run_stop_hook(
        &self,
        session: &SessionHandle,
        assistant_message: &str,
        stop_hook_active: bool,
    ) -> Result<Option<String>, SdkError> {
        let Some(hooks) = session.lifecycle_hooks().map_err(|error| {
            SdkError::internal_error().data(format!("Failed to read lifecycle hooks for ACP Stop hook: {error:#}"))
        })?
        else {
            return Ok(None);
        };
        let outcome = hooks
            .run_stop(assistant_message, stop_hook_active)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            warn_hook_message("Stop", &message);
        }
        Ok(outcome.block_reason)
    }
}
