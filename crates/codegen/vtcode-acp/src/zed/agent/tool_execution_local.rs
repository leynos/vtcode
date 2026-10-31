//! Implements workspace-local ACP file tools and converts their results for the Zed client.
mod list_files;

use super::super::types::SessionHandle;
use super::ZedAgent;
use super::tool_execution::cancelable_session_request;
use crate::acp;
use crate::reports::{
    TOOL_RESPONSE_KEY_CONTENT, TOOL_RESPONSE_KEY_CONTENT_HASH, TOOL_RESPONSE_KEY_PATH, TOOL_RESPONSE_KEY_STATUS,
    TOOL_RESPONSE_KEY_TOOL, TOOL_RESPONSE_KEY_TRUNCATED, TOOL_SUCCESS_LABEL, ToolExecutionReport, create_diff_content,
};
use crate::tooling::{TOOL_READ_FILE_LIMIT_ARG, TOOL_READ_FILE_LINE_ARG};
use crate::zed::connection::ConnectionHandle;
use anyhow::{Context, Result};
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
                let message = super::report_session_runtime_failure(&error);
                return ToolExecutionReport::failure(invocation.tool_name, &message);
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

    /// Admit and execute one public tool call, mapping the outcome into an ACP report.
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
                ToolExecutionReport::structured_failure(&error)
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

    /// Read a file through the ACP client and certify the version for stable bytes.
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

        // A certified version asserts that the returned window came from the
        // current on-disk bytes; it does not assert that the model saw the whole
        // file. The ACP client serves the editor buffer, which may be unsaved, so
        // a windowed read is certified only when its text equals the same line
        // window projected from the on-disk bytes (trailing `\n` ignored), and a
        // whole-file read only when the returned text hashes to those bytes. Any
        // mismatch, non-UTF-8 on-disk content, or failed read leaves the version
        // absent rather than certifying bytes the model never saw; `apply_patch`
        // treats an absent precondition as omitted.
        let expected_hash = if truncated {
            None
        } else if line.is_some() || limit.is_some() {
            match Self::windowed_disk_hash(&path, line, limit, &truncated_content).await {
                Ok(Some(content_hash)) => Some(content_hash),
                Ok(None) => {
                    warn!(path = ?path, "ACP read window does not match the on-disk file version");
                    None
                }
                Err(error) => {
                    warn!(%error, path = ?path, "Failed to verify the ACP read window against disk");
                    None
                }
            }
        } else {
            Some(format!("sha256:{}", vtcode_commons::utils::calculate_sha256(truncated_content.as_bytes())))
        };

        let content_hash = match expected_hash {
            None => Value::Null,
            Some(expected_hash) => {
                let record_result = match request.session.workspace_runtime() {
                    Ok(Some(runtime)) => {
                        runtime
                            .local_tool_registry
                            .file_ops_tool()
                            .record_read_content_hash_if_matches(&path, &expected_hash)
                            .await
                    }
                    Ok(None) => {
                        self.local_tool_registry
                            .file_ops_tool()
                            .record_read_content_hash_if_matches(&path, &expected_hash)
                            .await
                    }
                    Err(error) => Err(error),
                };
                match record_result {
                    Ok(Some(content_hash)) => json!(content_hash),
                    Ok(None) => {
                        warn!(path = ?path, "ACP read content does not match the on-disk file version");
                        Value::Null
                    }
                    Err(error) => {
                        warn!(%error, path = ?path, "Failed to capture ACP read file version");
                        Value::Null
                    }
                }
            }
        };

        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tools::READ_FILE,
            TOOL_RESPONSE_KEY_PATH: path.to_string_lossy(),
            TOOL_RESPONSE_KEY_CONTENT: truncated_content,
            TOOL_RESPONSE_KEY_CONTENT_HASH: content_hash,
            TOOL_RESPONSE_KEY_TRUNCATED: truncated,
        });

        let locations = vec![acp::ToolCallLocation::new(path.clone()).line(line)];

        Ok(ToolExecutionReport::success(vec![acp::ToolCallContent::from(tool_content)], locations, payload))
    }

    /// Hash the whole on-disk file when a returned line window matches its projection.
    ///
    /// Returns `Ok(None)` when the returned text is not the same window as the
    /// on-disk bytes. A read or UTF-8 error is propagated without including
    /// file content, so the caller can withhold the version and log a bounded
    /// warning.
    async fn windowed_disk_hash(
        path: &std::path::Path,
        line: Option<u32>,
        limit: Option<u32>,
        returned: &str,
    ) -> Result<Option<String>> {
        let disk_bytes = tokio::fs::read(path)
            .await
            .with_context(|| format!("Failed to read back file for version certification: {}", path.display()))?;
        let disk_text = std::str::from_utf8(&disk_bytes)
            .with_context(|| format!("File content is not valid UTF-8: {}", path.display()))?;
        let projected = vtcode_core::line_window(disk_text, line, limit);
        if !window_matches_projection(projected, returned) {
            return Ok(None);
        }
        Ok(Some(format!("sha256:{}", vtcode_commons::utils::calculate_sha256(&disk_bytes))))
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

/// Decide whether a returned read window is the on-disk projection.
///
/// `line_window` keeps line terminators, so the projections it produces end
/// with `\n` unless the file's last line is unterminated. The only tolerated
/// transport difference is a client dropping the terminator of the final line,
/// so `projected` may carry exactly one `\n` more than `returned` — and only
/// when `returned` is itself unterminated. A response that ends with `\n` is a
/// complete set of lines, so a shorter one has dropped a whole line; extra
/// blank lines are retained as well, because a client returning more text than
/// the window may be showing unsaved buffer content. Trimming every trailing
/// newline would accept such a buffer and certify disk bytes the model never
/// saw within its window, which is why the rule is deliberately asymmetric.
fn window_matches_projection(projected: &str, returned: &str) -> bool {
    if projected == returned {
        return true;
    }
    !returned.ends_with('\n') && projected.strip_suffix('\n') == Some(returned)
}

#[cfg(test)]
mod window_match_tests {
    use super::window_matches_projection;

    /// An identical window always matches.
    #[test]
    fn identical_windows_match() {
        assert!(window_matches_projection("alpha\n", "alpha\n"));
        assert!(window_matches_projection("", ""));
    }

    /// The client may omit the projection's single trailing terminator, but it
    /// may not add one: `returned` must never carry more than `projected`.
    #[test]
    fn one_trailing_newline_may_differ() {
        assert!(window_matches_projection("alpha\n", "alpha"));
        assert!(!window_matches_projection("alpha", "alpha\n"));
    }

    /// A response that is itself newline-terminated is a complete set of
    /// lines, so a shorter one has dropped a line rather than a terminator.
    #[test]
    fn a_shorter_complete_line_set_does_not_match() {
        assert!(!window_matches_projection("alpha\n\n", "alpha\n"));
        assert!(!window_matches_projection("alpha\nbeta\n", "alpha\n"));
    }

    /// Extra blank lines stay significant, because they can be unsaved edits.
    #[test]
    fn extra_blank_lines_do_not_match() {
        assert!(!window_matches_projection("alpha\n", "alpha\n\n"));
        assert!(!window_matches_projection("alpha\n\n", "alpha\n\n\n"));
        assert!(!window_matches_projection("alpha\n", "alpha\nbeta\n"));
    }

    /// A response that ignores the window and returns another line differs.
    #[test]
    fn a_wider_response_does_not_match() {
        assert!(!window_matches_projection("beta\n", "alpha\nbeta\n"));
        assert!(!window_matches_projection("beta\n", "beta\ngamma\n"));
    }
}
