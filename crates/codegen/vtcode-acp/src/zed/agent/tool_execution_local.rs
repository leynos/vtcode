//! Implements workspace-local ACP file tools and converts their results for the Zed client.
mod list_files;

use super::super::types::SessionHandle;
use super::ZedAgent;
use super::tool_execution::cancelable_session_request;
use crate::acp;
use crate::reports::{
    TOOL_RESPONSE_KEY_CONTENT, TOOL_RESPONSE_KEY_PATH, TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL,
    TOOL_RESPONSE_KEY_TRUNCATED, TOOL_SUCCESS_LABEL, ToolExecutionReport, create_diff_content,
};
use crate::tooling::{TOOL_READ_FILE_LIMIT_ARG, TOOL_READ_FILE_LINE_ARG};
use crate::zed::connection::ConnectionHandle;
use anyhow::Result;
use serde_json::{Value, json};
use tracing::warn;
use vtcode_core::config::constants::tools;
use vtcode_core::llm::provider::MessageRole;
use vtcode_core::tools::registry::{ExecutionPolicySnapshot, ToolExecutionRequest};
use vtcode_core::utils::ansi_parser::strip_ansi;

const RESTRICTED_LOCAL_TOOLS: &[&str] = &["debug_agent", "analyze_agent", "execute_code"];

/// Arguments shared by local-tool dispatch paths.
#[derive(Clone, Copy)]
pub(super) struct LocalToolInvocation<'a> {
    pub(super) tool_name: &'a str,
    pub(super) args: &'a Value,
    pub(super) call_id: &'a str,
}

pub(super) struct SessionLocalToolRequest<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) invocation: LocalToolInvocation<'a>,
}

pub(super) struct ClientFileReadRequest<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) client: &'a ConnectionHandle,
    pub(super) session_id: &'a acp::SessionId,
    pub(super) args: &'a Value,
}

impl ZedAgent {
    pub(super) async fn execute_local_tool(&self, tool_name: &str, args: &Value, call_id: &str) -> ToolExecutionReport {
        let invocation = LocalToolInvocation { tool_name, args, call_id };
        self.execute_local_tool_with_registry(&self.local_tool_registry, invocation)
            .await
    }

    pub(super) async fn execute_session_local_tool(&self, request: SessionLocalToolRequest<'_>) -> ToolExecutionReport {
        self.execute_session_local_invocation(request.session, request.invocation).await
    }

    async fn execute_session_local_invocation(
        &self,
        session: &SessionHandle,
        invocation: LocalToolInvocation<'_>,
    ) -> ToolExecutionReport {
        let runtime = match session.workspace_runtime() {
            Ok(runtime) => runtime,
            Err(error) => {
                return ToolExecutionReport::failure(
                    invocation.tool_name,
                    &format!("Failed to read ACP session workspace runtime: {error:#}"),
                );
            }
        };
        let registry = runtime
            .as_ref()
            .map_or(&self.local_tool_registry, |runtime| &runtime.local_tool_registry);
        let execution = self.execute_local_tool_with_registry(registry, invocation);
        if !vtcode_core::subagents::is_subagent_tool(invocation.tool_name) {
            return execution.await;
        }

        let parent_session_id = match session.data.lock() {
            Ok(data) => data.session_id.to_string(),
            Err(error) => {
                return ToolExecutionReport::failure(
                    invocation.tool_name,
                    &format!("Failed to read ACP session identifier for subagent invocation: {error}"),
                );
            }
        };
        let parent_messages = match self.resolved_messages(session) {
            Ok(messages) => messages,
            Err(error) => {
                return ToolExecutionReport::failure(
                    invocation.tool_name,
                    &format!("Failed to read ACP session messages for subagent invocation: {error:#}"),
                );
            }
        };
        let current_input = parent_messages
            .iter()
            .rev()
            .find(|message| message.role == MessageRole::User)
            .map(|message| message.content.as_text().into_owned())
            .unwrap_or_default();
        let context =
            vtcode_core::subagents::SubagentInvocationContext::new(parent_session_id, parent_messages, current_input);
        vtcode_core::subagents::with_subagent_invocation_context(context, execution).await
    }

    async fn execute_local_tool_with_registry(
        &self,
        registry: &vtcode_core::tools::registry::ToolRegistry,
        invocation: LocalToolInvocation<'_>,
    ) -> ToolExecutionReport {
        if RESTRICTED_LOCAL_TOOLS.contains(&invocation.tool_name) {
            warn!(tool = invocation.tool_name, "Attempted execution of restricted tool from external ACP client");
            return ToolExecutionReport::failure(
                invocation.tool_name,
                &format!("Tool '{}' is not available to external clients", invocation.tool_name),
            );
        }

        let prepared = match registry.admit_public_tool_call(invocation.tool_name, invocation.args) {
            Ok(prepared) => prepared,
            Err(error) => return ToolExecutionReport::failure(invocation.tool_name, &error.to_string()),
        };
        let policy = ExecutionPolicySnapshot::default()
            .with_invocation_id(Some(invocation.call_id.to_string()))
            .with_prevalidated(prepared.already_preflighted);
        let request = ToolExecutionRequest::new(prepared.canonical_name, prepared.effective_args).with_policy(policy);
        let outcome = registry.execute_public_tool_request(request).await;
        match (outcome.output, outcome.error) {
            (Some(output), None) => {
                let content = self.render_local_tool_content(invocation.tool_name, &output);
                let payload = json!({
                    TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
                    TOOL_RESPONSE_KEY_TOOL: invocation.tool_name,
                    "result": output.clone(),
                });
                ToolExecutionReport::success(content, Vec::new(), payload)
            }
            (_, Some(error)) => {
                let message = error.user_message();
                warn!(
                    error = %message,
                    tool = invocation.tool_name,
                    attempts = outcome.attempts,
                    "Failed to execute local tool"
                );
                ToolExecutionReport::failure(invocation.tool_name, &message)
            }
            (None, None) => {
                ToolExecutionReport::failure(invocation.tool_name, "Tool execution finished without output or error")
            }
        }
    }

    fn render_local_tool_content(&self, tool_name: &str, output: &Value) -> Vec<acp::ToolCallContent> {
        match tool_name {
            tools::EXEC_COMMAND | tools::RUN_PTY_CMD | tools::WRITE_STDIN => self.render_exec_command_content(output),
            tools::EDIT_FILE | tools::WRITE_FILE | tools::CREATE_FILE => Self::render_file_edit_content(output)
                .unwrap_or_else(|| self.render_text_tool_content(tool_name, output)),
            _ => self.render_text_tool_content(tool_name, output),
        }
    }

    fn render_file_edit_content(output: &Value) -> Option<Vec<acp::ToolCallContent>> {
        let path = output.get("path").and_then(Value::as_str)?;
        let new_text = output.get("new_text").and_then(Value::as_str)?;
        let old_text = output.get("old_text").and_then(Value::as_str);
        Some(vec![create_diff_content(path, old_text, new_text)])
    }

    fn render_text_tool_content(&self, tool_name: &str, output: &Value) -> Vec<acp::ToolCallContent> {
        let mut sections = self.render_stream_output_sections(output);
        if sections.is_empty() {
            sections = self.render_fallback_output_sections(output);
        }
        if sections.is_empty() {
            sections.push(format!("{tool_name} completed without output"));
        }

        vec![acp::ToolCallContent::from(sections.join("\n"))]
    }

    fn render_stream_output_sections(&self, output: &Value) -> Vec<String> {
        let mut sections = Vec::with_capacity(10);

        for (field, truncated_label) in [("stdout", "[stdout truncated]"), ("stderr", "[stderr truncated]")] {
            if let Some(value) = output.get(field).and_then(Value::as_str).filter(|value| !value.is_empty()) {
                let (rendered, truncated) = self.render_plain_text(value);
                sections.push(format!("{field}:\n{rendered}"));
                if truncated {
                    sections.push(truncated_label.to_string());
                }
            }
        }

        sections
    }

    fn render_fallback_output_sections(&self, output: &Value) -> Vec<String> {
        let (plain, truncated_label) =
            if let Some(message) = output.get("message").and_then(Value::as_str).filter(|value| !value.is_empty()) {
                (strip_ansi(message), "[message truncated]")
            } else {
                let summary = serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string());
                (strip_ansi(&summary), "[response truncated]")
            };
        let (rendered, truncated) = self.truncate_text(&plain);
        let mut sections = vec![rendered];
        if truncated {
            sections.push(truncated_label.to_string());
        }
        sections
    }

    fn render_plain_text(&self, output: &str) -> (String, bool) {
        let plain = strip_ansi(output);
        self.truncate_text(&plain)
    }

    pub(super) async fn run_read_file(
        &self,
        request: ClientFileReadRequest<'_>,
    ) -> Result<ToolExecutionReport, String> {
        let path = self.parse_tool_path(request.session, request.args)?;
        let line = Self::parse_positive_argument(request.args, TOOL_READ_FILE_LINE_ARG)?;
        let limit = Self::parse_positive_argument(request.args, TOOL_READ_FILE_LIMIT_ARG)?;

        let acp_request = acp::ReadTextFileRequest::new(request.session_id.clone(), path.clone())
            .line(line)
            .limit(limit);

        let response =
            match cancelable_session_request(&request.session.cancellation, request.client.read_text_file(acp_request))
                .await
            {
                Some(Ok(response)) => response,
                Some(Err(error)) => {
                    warn!(%error, path = ?path, "Failed to read file via ACP client");
                    return Err(format!("Unable to read file: {error}"));
                }
                None => return Ok(ToolExecutionReport::cancelled(tools::READ_FILE)),
            };

        let plain_response = strip_ansi(&response.content);
        let (truncated_content, truncated) = self.truncate_text(&plain_response);
        let mut tool_content = truncated_content.clone();
        if truncated {
            tool_content.push_str("\n\n[truncated]");
        }

        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tools::READ_FILE,
            TOOL_RESPONSE_KEY_PATH: path.to_string_lossy(),
            TOOL_RESPONSE_KEY_CONTENT: truncated_content,
            TOOL_RESPONSE_KEY_TRUNCATED: truncated,
        });

        let locations = vec![acp::ToolCallLocation::new(path.clone()).line(line)];

        Ok(ToolExecutionReport::success(vec![acp::ToolCallContent::from(tool_content)], locations, payload))
    }

    pub(crate) async fn run_list_files(&self, args: &Value) -> Result<ToolExecutionReport, String> {
        self.run_list_files_inner(args).await
    }

    pub(super) async fn run_session_list_files(
        &self,
        session: &SessionHandle,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        self.run_session_list_files_inner(session, args).await
    }
}
