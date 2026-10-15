//! Adapts the shared permission prompter to ACP permission requests.
use super::*;

#[async_trait]
impl<P> AcpPermissionPrompter for DefaultPermissionPrompter<P>
where
    P: ToolRegistryProvider + Send + Sync,
{
    fn permission_options(&self, tool: SupportedTool, args: Option<&Value>) -> Vec<acp::PermissionOption> {
        let action_label = self.render_action_label(tool, args);
        self.permission_options_for_action(&action_label)
    }

    async fn request_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_supported_tool_permission(
            client,
            SupportedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Standard,
            },
        )
        .await
    }

    async fn request_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: SupportedTool,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_supported_tool_permission(
            client,
            SupportedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Forced,
            },
        )
        .await
    }

    async fn request_named_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_named_tool_permission_impl(
            client,
            NamedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Standard,
            },
        )
        .await
    }

    async fn request_named_tool_permission_forced(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        self.request_named_tool_permission_impl(
            client,
            NamedToolPermissionRequest {
                session_id,
                call,
                tool,
                args,
                mode: PermissionPromptMode::Forced,
            },
        )
        .await
    }
}
