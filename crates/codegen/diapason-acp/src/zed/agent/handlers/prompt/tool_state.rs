//! Provider tool availability state for one ACP prompt.

use super::*;

/// Tool availability and definitions resolved for a single prompt.
pub(in crate::zed::agent::handlers) struct PromptToolState {
    pub(super) enabled_tools: Vec<SupportedTool>,
    pub(super) primary_agent: String,
    pub(super) has_local_tools: bool,
    pub(super) tools_allowed: bool,
    pub(super) tool_definitions: Option<Arc<Vec<ToolDefinition>>>,
    pub(super) messages: Vec<Message>,
}

impl PromptToolState {
    /// Create tool state for a prompt that has not resolved tools yet.
    pub(super) fn new(enabled_tools: Vec<SupportedTool>, primary_agent: String, provider_supports_tools: bool) -> Self {
        Self {
            enabled_tools,
            primary_agent,
            has_local_tools: false,
            tools_allowed: provider_supports_tools,
            tool_definitions: None,
            messages: Vec::new(),
        }
    }

    /// Re-resolve tool availability and definitions from the session.
    pub(super) fn refresh_availability(
        &mut self,
        agent: &ZedAgent,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        provider_supports_tools: bool,
    ) -> Result<(), SdkError> {
        self.primary_agent = session
            .data
            .lock()
            .map_err(|_err| SdkError::internal_error())?
            .primary_agent
            .clone();
        self.has_local_tools = agent
            .session_local_tools_available(session, &self.primary_agent)
            .map_err(|error| session_access_error(session_id, "checking local tool availability", error))?;
        self.tools_allowed = provider_supports_tools && (!self.enabled_tools.is_empty() || self.has_local_tools);
        self.tool_definitions = agent
            .session_tool_definitions(SessionToolDefinitionRequest {
                session,
                provider_supports_tools,
                enabled_tools: &self.enabled_tools,
                primary_agent: &self.primary_agent,
            })
            .map_err(|error| session_access_error(session_id, "resolving tool definitions", error))?
            .map(Arc::new);
        Ok(())
    }
}
