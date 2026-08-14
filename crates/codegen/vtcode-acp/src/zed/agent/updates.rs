use super::super::helpers::build_available_commands;
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::zed::connection::ConnectionHandle;
use anyhow::Result;
use vtcode_core::prompts::discover_prompt_templates;
use vtcode_core::ui::slash::visible_commands;

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

    pub(super) async fn send_available_commands_update(&self, session_id: &acp::SessionId) -> Result<(), SdkError> {
        let slash_commands = visible_commands();
        let workspace = self
            .session_handle(session_id)
            .and_then(|session| session.workspace_runtime())
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
