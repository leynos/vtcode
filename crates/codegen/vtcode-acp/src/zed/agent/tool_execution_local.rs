//! Implements workspace-local ACP file tools and converts their results for the Zed client.
use super::super::types::SessionHandle;
use super::ZedAgent;
use crate::acp;
use crate::reports::{
    TOOL_RESPONSE_KEY_CONTENT, TOOL_RESPONSE_KEY_PATH, TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL,
    TOOL_RESPONSE_KEY_TRUNCATED, TOOL_SUCCESS_LABEL, ToolExecutionReport, create_diff_content,
};
use crate::tooling::{
    TOOL_LIST_FILES_ITEMS_KEY, TOOL_LIST_FILES_MESSAGE_KEY, TOOL_LIST_FILES_PATH_ARG, TOOL_LIST_FILES_RESULT_KEY,
    TOOL_LIST_FILES_SUMMARY_MAX_ITEMS, TOOL_LIST_FILES_URI_ARG, TOOL_READ_FILE_LIMIT_ARG, TOOL_READ_FILE_LINE_ARG,
};
use crate::zed::connection::ConnectionHandle;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use tracing::warn;
use vtcode_core::config::constants::tools;
use vtcode_core::llm::provider::MessageRole;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::registry::{ExecutionPolicySnapshot, ToolExecutionRequest};
use vtcode_core::tools::traits::Tool;
use vtcode_core::utils::ansi_parser::strip_ansi;
use vtcode_core::utils::path::ensure_path_within_workspace;

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

        let response = request.client.read_text_file(acp_request).await.map_err(|error| {
            warn!(%error, path = ?path, "Failed to read file via ACP client");
            format!("Unable to read file: {error}")
        })?;

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
        self.run_list_files_for_tool(self.file_ops_tool.as_ref(), None, args).await
    }

    pub(super) async fn run_session_list_files(
        &self,
        session: &SessionHandle,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let runtime = match session.workspace_runtime() {
            Ok(Some(runtime)) => runtime,
            Ok(None) => return self.run_list_files(args).await,
            Err(error) => return Err(format!("Failed to read ACP session workspace runtime: {error:#}")),
        };
        self.run_list_files_for_tool(runtime.file_ops_tool.as_ref(), Some(session), args)
            .await
    }

    async fn run_list_files_for_tool(
        &self,
        tool: Option<&FileOpsTool>,
        session: Option<&SessionHandle>,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let Some(tool) = tool else {
            return Err("List files tool is unavailable".to_string());
        };

        let resolved_path = self.resolve_list_files_path(session, args)?.unwrap_or_else(|| ".".into());

        let mut normalized_args = match args.clone() {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        drop(normalized_args.insert(TOOL_LIST_FILES_PATH_ARG.to_string(), Value::String(resolved_path)));
        let normalized_args = Value::Object(normalized_args);

        let listing = tool.execute(normalized_args).await.map_err(|error| {
            let detail = error.to_string();
            warn!(error = %detail, "Failed to execute list_files tool");
            format!("Unable to list files: {detail}")
        })?;

        let content = Self::list_files_content(&listing);
        let locations = Self::list_files_locations(&listing);
        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tools::LIST_FILES,
            TOOL_LIST_FILES_RESULT_KEY: listing,
        });

        Ok(ToolExecutionReport::success(content, locations, payload))
    }

    fn resolve_list_files_path(&self, session: Option<&SessionHandle>, args: &Value) -> Result<Option<String>, String> {
        if let Some(path) = args
            .get(TOOL_LIST_FILES_PATH_ARG)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            return Ok(Some(path.to_string()));
        }

        if let Some(uri) = args
            .get(TOOL_LIST_FILES_URI_ARG)
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
        {
            let resolved = if let Some(session) = session {
                self.parse_resource_path(session, uri)?
            } else {
                let candidate = Self::decode_resource_uri(uri)?;
                self.resolve_workspace_path(candidate, TOOL_LIST_FILES_URI_ARG)?
            };
            let workspace_runtime = match session {
                Some(session) => session
                    .workspace_runtime()
                    .map_err(|error| format!("Failed to read ACP session workspace runtime: {error:#}"))?,
                None => None,
            };
            let workspace_root = workspace_runtime
                .as_ref()
                .map_or_else(|| self.workspace_root(), |runtime| runtime.workspace_root.as_path());
            let normalized = ensure_path_within_workspace(&resolved, workspace_root)
                .map_err(|_err| "list_files path must stay within the workspace".to_string())?;

            if normalized == workspace_root {
                return Ok(Some(".".into()));
            }

            if let Ok(relative) = normalized.strip_prefix(workspace_root) {
                if relative.as_os_str().is_empty() {
                    return Ok(Some(".".into()));
                }
                return Ok(Some(relative.to_string_lossy().into()));
            }

            return Ok(Some(normalized.to_string_lossy().into()));
        }

        Ok(None)
    }

    fn list_files_content(output: &Value) -> Vec<acp::ToolCallContent> {
        let mut lines = Vec::new();

        if let (Some(count), Some(total)) =
            (output.get("count").and_then(Value::as_u64), output.get("total").and_then(Value::as_u64))
        {
            lines.push(format!("Showing {count} of {total} items"));
        }

        if let Some(items) = output.get(TOOL_LIST_FILES_ITEMS_KEY).and_then(Value::as_array) {
            if items.is_empty() {
                lines.push("No items found.".to_string());
            } else {
                for item in items.iter().take(TOOL_LIST_FILES_SUMMARY_MAX_ITEMS) {
                    let path = item
                        .get("path")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("name").and_then(Value::as_str))
                        .unwrap_or("(unknown)");
                    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("file");
                    let prefix = match item_type {
                        "directory" => "[dir]",
                        "file" => "[file]",
                        other => other,
                    };
                    lines.push(format!("{prefix} {path}"));
                }

                if items.len() > TOOL_LIST_FILES_SUMMARY_MAX_ITEMS {
                    let remaining = items.len() - TOOL_LIST_FILES_SUMMARY_MAX_ITEMS;
                    lines.push(format!("… and {remaining} more"));
                }
            }
        } else {
            lines.push("No results returned.".to_string());
        }

        if let Some(has_more) = output.get("has_more").and_then(Value::as_bool)
            && has_more
        {
            lines.push("Additional results available (adjust page or per_page to view more).".to_string());
        }

        if let Some(message) = output
            .get(TOOL_LIST_FILES_MESSAGE_KEY)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            lines.push(message.to_string());
        }

        if lines.is_empty() {
            lines.push("No results.".to_string());
        }

        vec![acp::ToolCallContent::from(lines.join(" "))]
    }

    fn list_files_locations(output: &Value) -> Vec<acp::ToolCallLocation> {
        let Some(items) = output.get(TOOL_LIST_FILES_ITEMS_KEY).and_then(Value::as_array) else {
            return Vec::new();
        };

        items
            .iter()
            .filter_map(|item| {
                item.get("path")
                    .or_else(|| item.get("name"))
                    .and_then(Value::as_str)
                    .map(PathBuf::from)
            })
            .take(TOOL_LIST_FILES_SUMMARY_MAX_ITEMS)
            .map(acp::ToolCallLocation::new)
            .collect()
    }
}
