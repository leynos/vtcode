//! ACP session lifecycle hooks, close semantics, and workspace release.
use super::*;
use std::time::Duration;
use vtcode_core::hooks::SessionEndReason;

impl ZedAgent {
    pub(crate) async fn run_session_start_hooks(&self, session: &SessionHandle) -> anyhow::Result<()> {
        if !session.mark_session_started().context("checking ACP session start state")? {
            return Ok(());
        }

        session
            .update_transcript_path()
            .await
            .context("updating ACP session transcript path")?;
        let Some(hooks) = session.lifecycle_hooks().context("reading ACP session lifecycle hooks")? else {
            return Ok(());
        };
        let workspace = session
            .workspace_runtime()
            .context("reading ACP session workspace runtime for SessionStart hooks")?
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        vtcode_core::hooks::restore_workspace_hook_approval(&hooks, &workspace).await;
        let outcome = hooks.run_session_start().await?;
        for message in outcome.messages {
            warn!(level = ?message.level, message_bytes = message.text.len(), "ACP SessionStart hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                self.push_message(session, Message::system(context))
                    .context("appending SessionStart hook context to the ACP session")?;
            }
        }
        Ok(())
    }

    pub(crate) async fn run_session_end_hooks(&self, reason: SessionEndReason) -> anyhow::Result<()> {
        let sessions = self
            .sessions
            .lock()
            .map_err(|error| lock_poison_error("listing ACP sessions for SessionEnd hooks", error))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut first_error = None;
        for session in sessions {
            if let Err(error) = Self::run_session_end_hooks_for(&session, reason).await {
                let _ = first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    async fn run_session_end_hooks_for(session: &SessionHandle, reason: SessionEndReason) -> anyhow::Result<()> {
        let turn_id = match session.mark_session_ended() {
            Ok(Some(turn_id)) => turn_id,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(%error, "Failed to read ACP session end state; skipping lifecycle hooks");
                return Err(error.context("checking ACP session end state"));
            }
        };
        let hooks = match session.lifecycle_hooks() {
            Ok(Some(hooks)) => hooks,
            Ok(None) => return Ok(()),
            Err(error) => {
                warn!(%error, "Failed to read ACP session lifecycle hooks at shutdown");
                return Err(error.context("reading ACP session lifecycle hooks at shutdown"));
            }
        };

        match hooks.run_session_end(&turn_id, reason).await {
            Ok(messages) => {
                for message in messages {
                    warn!(level = ?message.level, message_bytes = message.text.len(), "ACP SessionEnd hook");
                }
            }
            Err(error) => warn!(%error, "ACP SessionEnd hooks failed"),
        }
        Ok(())
    }

    pub(crate) async fn close_session(
        &self,
        session_id: &acp::SessionId,
    ) -> Result<acp::CloseSessionResponse, acp::Error> {
        let session = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|error| acp::Error::internal_error().data(format!("Failed to close ACP session: {error}")))?;
            let session = sessions
                .remove(session_id)
                .ok_or_else(|| acp::Error::invalid_params().data(serde_json::json!({ "reason": "unknown_session" })))?;
            session.begin_close();
            session
        };

        let close_task =
            tokio::spawn(async move { Self::finish_closed_session(&session, SessionEndReason::Exit).await });
        match tokio::time::timeout(Duration::from_secs(3), close_task).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(error))) => {
                warn!(%error, session_id = %session_id, "Failed to finish ACP session cleanup after close");
                return Err(acp::Error::internal_error().data(error.to_string()));
            }
            Ok(Err(error)) => {
                warn!(%error, session_id = %session_id, "ACP session close cleanup task failed");
                return Err(acp::Error::internal_error().data(error.to_string()));
            }
            Err(error) => {
                warn!(%error, session_id = %session_id, "ACP session close exceeded its wait bound; cleanup continues");
                return Err(acp::Error::internal_error()
                    .data("ACP session close timed out waiting for prompt work; cleanup continues"));
            }
        }
        debug!(session_id = %session_id, "Closed ACP session after prompt work quiesced");
        Ok(acp::CloseSessionResponse::new())
    }

    pub(crate) async fn close_all_sessions(&self, reason: SessionEndReason) -> anyhow::Result<()> {
        let sessions = {
            let mut sessions = self
                .sessions
                .lock()
                .map_err(|error| lock_poison_error("closing ACP sessions after connection end", error))?;
            sessions
                .drain()
                .map(|(_, session)| {
                    session.begin_close();
                    session
                })
                .collect::<Vec<_>>()
        };

        let mut first_error = None;
        for session in sessions {
            if let Err(error) = Self::finish_closed_session(&session, reason).await {
                let _ = first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    pub(super) async fn discard_failed_new_session(&self, session_id: &acp::SessionId) {
        if let Err(error) = self.close_session(session_id).await {
            warn!(%error, session_id = %session_id, "Failed to clean up ACP session after initialization error");
        }
    }

    async fn finish_closed_session(session: &SessionHandle, reason: SessionEndReason) -> anyhow::Result<()> {
        session.wait_for_prompts().await;

        let mut first_error = None;
        if let Err(error) = session.stop_task_lifecycle_forwarder().await {
            let _ = first_error.get_or_insert(error);
        }
        if let Err(error) = Self::run_session_end_hooks_for(session, reason).await {
            let _ = first_error.get_or_insert(error);
        }
        if let Err(error) = session.release_workspace_lease().await {
            let _ = first_error.get_or_insert(error);
        }
        first_error.map_or(Ok(()), Err)
    }
}
