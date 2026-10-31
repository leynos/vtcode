//! Sends session updates and derives ACP command and prompt-template listings for the client.
use super::super::helpers::{build_available_commands, text_chunk};
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::agent::log_suppressed_error;
use crate::zed::connection::ConnectionHandle;
use crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE;
use anyhow::Result;
use vtcode_core::prompts::discover_prompt_templates;
use vtcode_core::ui::slash::visible_commands;

/// The user-facing channel carried by an assistant update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AssistantChunkKind {
    Reasoning,
    Commentary,
}

impl AssistantChunkKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Reasoning => "agent_thought_chunk",
            Self::Commentary => "agent_message_chunk",
        }
    }
}

/// Builds the ordered user-facing updates for one provider response.
///
/// Reasoning travels as a thought chunk ahead of commentary so clients that
/// render updates in arrival order show the model's intent before its answer.
/// Empty channels produce no update, and no text is trimmed or rewritten.
pub(super) fn assistant_updates(content: &str, reasoning: &str) -> Vec<(AssistantChunkKind, acp::SessionUpdate)> {
    let mut updates = Vec::with_capacity(2);
    if !reasoning.is_empty() {
        updates.push((
            AssistantChunkKind::Reasoning,
            acp::SessionUpdate::AgentThoughtChunk(text_chunk(reasoning.to_owned())),
        ));
    }
    if !content.is_empty() {
        updates.push((
            AssistantChunkKind::Commentary,
            acp::SessionUpdate::AgentMessageChunk(text_chunk(content.to_owned())),
        ));
    }
    updates
}

fn log_assistant_update(
    kind: AssistantChunkKind,
    session_id: &acp::SessionId,
    loop_iteration: usize,
    content: &str,
    reasoning: &str,
    has_tool_calls: bool,
    tool_call_count: usize,
) {
    match kind {
        AssistantChunkKind::Reasoning => tracing::debug!(
            session_id = %session_id,
            loop_iteration,
            reasoning_bytes = reasoning.len(),
            has_tool_calls,
            tool_call_count,
            "Sending provider reasoning to ACP client"
        ),
        AssistantChunkKind::Commentary => tracing::debug!(
            session_id = %session_id,
            loop_iteration,
            content_bytes = content.len(),
            has_tool_calls,
            tool_call_count,
            "Sending assistant commentary to ACP client"
        ),
    }
}

impl ZedAgent {
    pub(super) async fn send_update(
        &self,
        session_id: &acp::SessionId,
        update: acp::SessionUpdate,
    ) -> Result<(), SdkError> {
        let Some(client) = self.client() else {
            return Err(SdkError::internal_error());
        };
        let notification = acp::SessionNotification::new(session_id.clone(), update);
        ConnectionHandle::send_session_notification(&client, notification)
    }

    /// Relays one provider response's reasoning and commentary to the ACP client.
    ///
    /// Thought chunks are sent before message chunks. Delivery failures do not
    /// abort the turn: each failure is warned with its session and channel so
    /// the remaining update still reaches the client.
    pub(super) async fn send_assistant_updates(
        &self,
        session_id: &acp::SessionId,
        loop_iteration: usize,
        content: &str,
        reasoning: &str,
        tool_call_count: usize,
    ) {
        let has_tool_calls = tool_call_count > 0;
        let updates = assistant_updates(content, reasoning);
        if updates.is_empty() {
            if has_tool_calls {
                tracing::debug!(
                    session_id = %session_id,
                    loop_iteration,
                    has_tool_calls,
                    tool_call_count,
                    "Tool-bearing provider response carried no assistant commentary or reasoning"
                );
            }
            return;
        }

        for (kind, update) in updates {
            log_assistant_update(kind, session_id, loop_iteration, content, reasoning, has_tool_calls, tool_call_count);
            if let Err(error) = self.send_update(session_id, update).await {
                tracing::warn!(
                    session_id = %session_id,
                    update_kind = kind.as_str(),
                    error = %error,
                    "Failed to send assistant update to ACP client"
                );
            }
        }
    }

    pub(super) async fn send_available_commands_update(&self, session_id: &acp::SessionId) -> Result<(), SdkError> {
        let slash_commands = visible_commands();
        let workspace_runtime = self
            .session_handle(session_id)
            .map_err(|error| {
                SdkError::internal_error().data(format!("Failed to look up ACP session {session_id}: {error:#}"))
            })?
            .map(|session| {
                session.workspace_runtime().map_err(|error| {
                    log_suppressed_error(&error, "session_workspace_runtime_read", "session_lock_unavailable");
                    SdkError::internal_error().data(SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE)
                })
            })
            .transpose()?;
        let workspace = workspace_runtime
            .flatten()
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        let prompt_templates = discover_prompt_templates(&workspace).await;
        let available_commands = build_available_commands(&slash_commands, &prompt_templates);

        tracing::debug!(
            session_id = %session_id,
            command_count = available_commands.len(),
            slash_command_count = slash_commands.len(),
            template_count = prompt_templates.len(),
            "Sending available commands update to ACP client"
        );

        self.send_update(
            session_id,
            acp::SessionUpdate::AvailableCommandsUpdate(acp::AvailableCommandsUpdate::new(available_commands)),
        )
        .await
    }
}
