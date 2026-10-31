//! Applies ACP turn lifecycle hooks and commits completed assistant responses.
use super::super::super::{warn_hook_block_reason, warn_hook_message};
use super::*;

impl PromptExecution {
    pub(super) async fn finish(mut self, stop_reason: acp::StopReason) -> Result<PromptResponse, SdkError> {
        if stop_reason != acp::StopReason::Cancelled {
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
            self.emit_final_assistant_updates().await;
            self.persist_completed_assistant_message().await?;
        }
        Ok(PromptResponse::new(stop_reason))
    }

    async fn emit_final_assistant_updates(&self) {
        if !should_emit_buffered_final_chunk(self.allow_streaming)
            || (self.assistant_message.is_empty() && self.assistant_reasoning.is_empty())
        {
            return;
        }
        self.agent
            .send_assistant_updates(
                &self.args.session_id,
                self.provider_iteration,
                &self.assistant_message,
                &self.assistant_reasoning,
                0,
            )
            .await;
    }

    async fn persist_completed_assistant_message(&mut self) -> Result<(), SdkError> {
        if self.assistant_message.is_empty() {
            return Ok(());
        }
        let mut completed_message = Message::assistant(std::mem::take(&mut self.assistant_message));
        if !self.assistant_reasoning.is_empty() {
            completed_message.reasoning = Some(std::mem::take(&mut self.assistant_reasoning));
        }
        push_session_message(&self.agent, &self.session, &self.args.session_id, completed_message)?;
        persist_session_checkpoint(&self.agent, &self.session, "assistant_response").await;
        Ok(())
    }
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
