//! Dispatch ACP local and protocol-supported terminal tools.
use super::super::ZedAgent;
use super::cancelable_session_request;
use super::data::ToolExecutionContext;
use crate::acp;
use crate::reports::{TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL, TOOL_SUCCESS_LABEL, ToolExecutionReport};
use crate::tooling::{SupportedTool, ToolDescriptor};
use crate::zed::types::RunTerminalMode;
use serde_json::{Value, json};
use vtcode_core::config::constants::tools;

use super::super::tool_execution_local::{ClientFileReadRequest, LocalToolInvocation, SessionLocalToolRequest};

impl ZedAgent {
    pub(super) async fn execute_descriptor(
        &self,
        context: &ToolExecutionContext<'_>,
        descriptor: ToolDescriptor,
        args: &Value,
    ) -> ToolExecutionReport {
        let tool_name = Self::tool_name_from_call(context.invocation.call);
        if should_route_terminal_via_client(tool_name, args) {
            if let Some(report) = self.execute_terminal_via_client(context, args).await {
                return report;
            }
        }

        match descriptor {
            ToolDescriptor::Acp(tool) => self.execute_acp_tool(context, tool, args).await,
            ToolDescriptor::Local => {
                self.execute_session_local_tool(SessionLocalToolRequest {
                    session: context.invocation.session,
                    invocation: LocalToolInvocation {
                        tool_name,
                        args,
                        call_id: context.invocation.call.id.as_str(),
                    },
                })
                .await
            }
        }
    }

    async fn execute_terminal_via_client(
        &self,
        context: &ToolExecutionContext<'_>,
        args: &Value,
    ) -> Option<ToolExecutionReport> {
        let tool_name = Self::tool_name_from_call(context.invocation.call);
        match self.client_supports_terminal() {
            Ok(true) => {}
            Ok(false) => return None,
            Err(error) => {
                return Some(ToolExecutionReport::failure(
                    tool_name,
                    &format!("Failed to read ACP client terminal capabilities: {error:#}"),
                ));
            }
        }

        match Self::requested_terminal_mode(args) {
            Ok(RunTerminalMode::Terminal) => None,
            Ok(RunTerminalMode::Pty) => Some(match self.launch_client_terminal(context, args).await {
                Ok(report) => report,
                Err(message) => ToolExecutionReport::failure(tool_name, &message),
            }),
            Err(message) => Some(ToolExecutionReport::failure(tool_name, &message)),
        }
    }

    async fn launch_client_terminal(
        &self,
        context: &ToolExecutionContext<'_>,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let tool_name = Self::tool_name_from_call(context.invocation.call);
        let command_parts = Self::parse_terminal_command(args)?;
        let (program, rest) = command_parts
            .split_first()
            .ok_or_else(|| "command array cannot be empty".to_string())?;

        let working_dir = self.resolve_session_terminal_working_dir(context.invocation.session, args)?;
        let location_display =
            self.describe_session_terminal_location(context.invocation.session, working_dir.as_ref())?;
        let command_display = command_parts.join(" ");

        let request = acp::CreateTerminalRequest::new(context.invocation.session_id.clone(), program.to_string())
            .args(rest.to_vec())
            .cwd(working_dir.clone());

        let response = match cancelable_session_request(
            &context.invocation.session.cancellation,
            context.invocation.client.create_terminal(request),
        )
        .await
        {
            Some(Ok(response)) => response,
            Some(Err(error)) => return Err(format!("Failed to create terminal: {error}")),
            None => return Ok(ToolExecutionReport::cancelled(tool_name)),
        };
        let terminal_id = response.terminal_id;

        let mut content = Vec::with_capacity(2);
        let summary = match location_display.as_deref() {
            Some(".") | None => format!("Started terminal command: {command_display}"),
            Some(location) => {
                format!("Started terminal command in {location}: {command_display}")
            }
        };
        content.push(acp::ToolCallContent::from(summary));
        content.push(acp::ToolCallContent::Terminal(acp::Terminal::new(terminal_id.clone())));

        let payload = json!({
            TOOL_RESPONSE_KEY_STATUS: TOOL_SUCCESS_LABEL,
            TOOL_RESPONSE_KEY_TOOL: tool_name,
            "result": {
                "terminal_id": terminal_id.to_string(),
                "mode": "pty",
                "command": command_parts,
                "working_dir": location_display,
            }
        });

        Ok(ToolExecutionReport::success(content, Vec::new(), payload))
    }

    async fn execute_acp_tool(
        &self,
        context: &ToolExecutionContext<'_>,
        tool: SupportedTool,
        args: &Value,
    ) -> ToolExecutionReport {
        match tool {
            SupportedTool::ReadFile => self
                .run_read_file(ClientFileReadRequest {
                    session: context.invocation.session,
                    client: context.invocation.client,
                    session_id: context.invocation.session_id,
                    args,
                })
                .await
                .unwrap_or_else(|message| ToolExecutionReport::failure(tools::READ_FILE, &message)),
            SupportedTool::ListFiles => self
                .run_session_list_files(context.invocation.session, args)
                .await
                .unwrap_or_else(|message| ToolExecutionReport::failure(tools::LIST_FILES, &message)),
        }
    }
}

fn should_route_terminal_via_client(tool_name: &str, _args: &Value) -> bool {
    matches!(tool_name, tools::RUN_PTY_CMD | tools::EXEC_COMMAND)
}
