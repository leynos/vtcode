use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::audit::timestamp_unix_ms;
use crate::permissions::PermissionToolContext;
use crate::reports::{
    TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL, TOOL_SUCCESS_LABEL, ToolExecutionReport, create_diff_content,
};
use crate::tooling::{SupportedTool, ToolDescriptor};
use crate::zed::connection::ConnectionHandle;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant as StdInstant;
use tokio::time::Instant;
use vtcode_commons::fs::canonicalize_with_context_async;
use vtcode_core::config::constants::tools;
use vtcode_core::hooks::{PermissionDecisionBehavior, PreToolHookDecision};
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;
use vtcode_core::permissions::build_permission_request;
use vtcode_core::tools::apply_patch::{Patch, PatchOperation, decode_apply_patch_input};

use super::super::types::{RunTerminalMode, SessionHandle, ToolCallResult};

impl ZedAgent {
    pub(super) async fn execute_tool_calls(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        calls: &[ProviderToolCall],
    ) -> Result<Vec<ToolCallResult>, SdkError> {
        if calls.is_empty() {
            return Ok(Vec::new());
        }

        let turn_id = uuid::Uuid::new_v4().to_string();
        let Some(client) = self.client() else {
            let mut results = Vec::with_capacity(calls.len());
            for call in calls {
                let report =
                    ToolExecutionReport::failure(Self::tool_name_from_call(call), "Client connection unavailable");
                let result = Self::tool_call_result_from_report(call, report);
                self.record_tool_audit(session_id, &turn_id, call, Ok((&result.llm_response, result.audit_status)), 0)
                    .await;
                results.push(result);
            }
            return Ok(results);
        };

        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            self.pace_tool_call(session).await;
            results.push(
                self.execute_tool_call(session, session_id, &turn_id, call, client.as_ref())
                    .await?,
            );
        }

        Ok(results)
    }

    async fn execute_tool_call(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        turn_id: &str,
        call: &ProviderToolCall,
        client: &ConnectionHandle,
    ) -> Result<ToolCallResult, SdkError> {
        let started = StdInstant::now();
        let result = self.execute_tool_call_inner(session, session_id, call, client).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_tool_audit(
            session_id,
            turn_id,
            call,
            match &result {
                Ok(result) => Ok((&result.llm_response, result.audit_status)),
                Err(_) => Err("ACP tool execution failed"),
            },
            duration_ms,
        )
        .await;
        result
    }

    async fn record_tool_audit(
        &self,
        session_id: &acp::SessionId,
        turn_id: &str,
        call: &ProviderToolCall,
        result: Result<(&str, vtcode_safety::audit_log::ToolAuditStatus), &str>,
        duration_ms: u64,
    ) {
        let Some(logger) = self.audit_logger.as_ref() else {
            return;
        };
        let tool_name = Self::tool_name_from_call(call);
        let original_args = call
            .function
            .as_ref()
            .map_or_else(|| b"".to_vec(), |function| function.arguments.as_bytes().to_vec());
        let (status, reason, result_bytes) = match result {
            Ok((output, status)) => {
                let reason = match status {
                    vtcode_safety::audit_log::ToolAuditStatus::Blocked => Some("denied".to_owned()),
                    vtcode_safety::audit_log::ToolAuditStatus::Cancelled => Some("cancelled".to_owned()),
                    _ => None,
                };
                (status, reason, output.as_bytes().to_vec())
            }
            Err(_) => (vtcode_safety::audit_log::ToolAuditStatus::Failure, None, Vec::new()),
        };
        let entry =
            vtcode_safety::audit_log::ToolAuditEntry::from_invocation(vtcode_safety::audit_log::ToolAuditInvocation {
                timestamp_unix_ms: timestamp_unix_ms(),
                session_id: &session_id.to_string(),
                turn_id,
                tool_call_id: &call.id,
                tool_name,
                arguments: &original_args,
                result: &result_bytes,
                duration_ms,
                status,
                model_id: Some(&self.config.model),
                reason: reason.as_deref(),
            });
        if let Err(error) = logger.record(entry).await {
            tracing::warn!(%error, tool = tool_name, call_id = %call.id, "Failed to write ACP tool audit entry");
        }
    }

    async fn execute_tool_call_inner(
        &self,
        session: &SessionHandle,
        session_id: &acp::SessionId,
        call: &ProviderToolCall,
        client: &ConnectionHandle,
    ) -> Result<ToolCallResult, SdkError> {
        let Some(func_ref) = call.function.as_ref() else {
            return Ok(Self::tool_call_result_from_report(
                call,
                ToolExecutionReport::failure("unknown", "Malformed tool call: missing function payload"),
            ));
        };
        let tool_descriptor = self.acp_tool_registry.lookup(&func_ref.name);
        let args_value_result: Result<Value, _> = serde_json::from_str(&func_ref.arguments);
        let args_value_for_input = args_value_result.as_ref().ok().cloned();
        let title = match (tool_descriptor, args_value_for_input.as_ref()) {
            (Some(descriptor), Some(args)) => self.acp_tool_registry.render_title(descriptor, &func_ref.name, args),
            (Some(descriptor), None) => {
                let null_args = Value::Null;
                self.acp_tool_registry.render_title(descriptor, &func_ref.name, &null_args)
            }
            (None, _) => format!("{} (unsupported)", func_ref.name),
        };

        let call_id = acp::ToolCallId::new(Arc::from(call.id.clone()));
        let kind = match tool_descriptor {
            Some(ToolDescriptor::Acp(tool)) => tool.kind(),
            Some(ToolDescriptor::Local) | None => self
                .acp_tool_registry
                .tool_kind_for_call(&func_ref.name, args_value_for_input.as_ref()),
        };
        let initial_call = acp::ToolCall::new(call_id.clone(), title)
            .kind(kind)
            .status(acp::ToolCallStatus::Pending)
            .content(Self::initial_tool_content(&func_ref.name, args_value_for_input.as_ref()))
            .raw_input(args_value_for_input.clone());

        let pre_tool_decision = if let Some(hooks) = session.lifecycle_hooks() {
            let outcome = hooks
                .run_pre_tool_use(&func_ref.name, args_value_for_input.as_ref(), Some(call.id.as_str()))
                .await
                .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
            for message in outcome.messages {
                tracing::warn!(level = ?message.level, message = %message.text, "ACP PreToolUse hook");
            }
            outcome.decision
        } else {
            PreToolHookDecision::Continue
        };

        self.send_update(session_id, acp::SessionUpdate::ToolCall(initial_call.clone()))
            .await?;

        if matches!(pre_tool_decision, PreToolHookDecision::Deny) {
            let report = ToolExecutionReport::blocked(&func_ref.name, "Tool execution denied by PreToolUse hook");
            let update = acp::ToolCallUpdate::new(call_id, Self::update_fields_from_report(&report));
            self.send_update(session_id, acp::SessionUpdate::ToolCallUpdate(update)).await?;
            return Ok(Self::tool_call_result_from_report(call, report));
        }

        let cancel_active = session.cancellation.is_cancelled();
        let permission_hook_decision = if let (false, Some(descriptor), Some(args_value)) =
            (cancel_active, tool_descriptor, args_value_for_input.as_ref())
        {
            self.run_permission_hook(session, &func_ref.name, descriptor, args_value)
                .await?
        } else {
            None
        };
        let mut effective_args = args_value_for_input.clone();
        let permission_override = if let Some(decision) = permission_hook_decision {
            if let Some(updated_input) = decision.updated_input.clone() {
                effective_args = Some(updated_input);
            }
            if decision.scope != vtcode_core::hooks::PermissionDecisionScope::Once {
                tracing::warn!(tool = %func_ref.name, scope = ?decision.scope, "ACP PermissionRequest hook scope is not persisted");
            }
            if !decision.permission_updates.is_empty() {
                tracing::warn!(tool = %func_ref.name, "ACP PermissionRequest hook permission updates are unsupported");
            }
            if decision.interrupt {
                tracing::warn!(tool = %func_ref.name, "ACP PermissionRequest hook interrupted tool execution");
                Some(ToolExecutionReport::failure(
                    &func_ref.name,
                    "Tool execution interrupted by PermissionRequest hook",
                ))
            } else {
                match decision.behavior {
                    PermissionDecisionBehavior::Allow => None,
                    PermissionDecisionBehavior::Deny => Some(ToolExecutionReport::blocked(
                        &func_ref.name,
                        "Tool execution denied by PermissionRequest hook",
                    )),
                }
            }
        } else if matches!(pre_tool_decision, PreToolHookDecision::Allow) {
            None
        } else if let (false, Some(descriptor), Some(args_value)) =
            (cancel_active, tool_descriptor, effective_args.as_ref())
        {
            let force_prompt = matches!(pre_tool_decision, PreToolHookDecision::Ask);
            match descriptor {
                ToolDescriptor::Acp(tool) if force_prompt => {
                    self.permission_prompter
                        .request_tool_permission_forced(client, session_id, &initial_call, tool, args_value)
                        .await?
                }
                ToolDescriptor::Acp(tool) => {
                    self.permission_prompter
                        .request_tool_permission(client, session_id, &initial_call, tool, args_value)
                        .await?
                }
                ToolDescriptor::Local if force_prompt => {
                    self.permission_prompter
                        .request_named_tool_permission_forced(
                            client,
                            session_id,
                            &initial_call,
                            PermissionToolContext::new(&func_ref.name, kind, initial_call.title.as_str()),
                            args_value,
                        )
                        .await?
                }
                ToolDescriptor::Local => {
                    self.permission_prompter
                        .request_named_tool_permission(
                            client,
                            session_id,
                            &initial_call,
                            PermissionToolContext::new(&func_ref.name, kind, initial_call.title.as_str()),
                            args_value,
                        )
                        .await?
                }
            }
        } else {
            None
        };

        let cancel_after_permission = session.cancellation.is_cancelled();
        if tool_descriptor.is_some() && permission_override.is_none() && !cancel_after_permission {
            let in_progress_fields = acp::ToolCallUpdateFields::default().status(acp::ToolCallStatus::InProgress);
            let progress_update = acp::ToolCallUpdate::new(call_id.clone(), in_progress_fields);
            self.send_update(session_id, acp::SessionUpdate::ToolCallUpdate(progress_update))
                .await?;
        }

        let apply_patch_snapshot =
            if permission_override.is_none() && !cancel_after_permission && func_ref.name == tools::APPLY_PATCH {
                self.capture_apply_patch_snapshot(effective_args.as_ref()).await
            } else {
                None
            };

        let mut report = if let Some(report) = permission_override {
            report
        } else if cancel_after_permission {
            ToolExecutionReport::cancelled(&func_ref.name)
        } else {
            match (tool_descriptor, effective_args.as_ref()) {
                (Some(descriptor), Some(args_value)) => {
                    self.execute_descriptor(
                        descriptor,
                        &func_ref.name,
                        client,
                        session_id,
                        call.id.as_str(),
                        args_value,
                    )
                    .await
                }
                (None, Some(_)) => ToolExecutionReport::failure(&func_ref.name, "Unsupported tool"),
                (Some(_), None) => {
                    let error = match args_value_result.as_ref() {
                        Ok(_) => "Invalid JSON arguments".to_string(),
                        Err(error) => format!("Invalid JSON arguments: {error}"),
                    };
                    ToolExecutionReport::failure(&func_ref.name, &error)
                }
                (None, None) => ToolExecutionReport::failure(&func_ref.name, "Invalid JSON arguments"),
            }
        };

        if session.cancellation.is_cancelled() && matches!(report.status, acp::ToolCallStatus::Completed) {
            report = ToolExecutionReport::cancelled(&func_ref.name);
        }

        attach_apply_patch_diff_content(&mut report, apply_patch_snapshot.as_ref()).await;

        if should_run_post_tool_hook(&report.status)
            && let Some(hooks) = session.lifecycle_hooks()
        {
            let output = report
                .raw_output
                .as_ref()
                .cloned()
                .unwrap_or_else(|| Value::String(report.llm_response.clone()));
            let outcome = hooks
                .run_post_tool_use(&func_ref.name, effective_args.as_ref(), &output, Some(call.id.as_str()))
                .await
                .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
            for message in outcome.messages {
                tracing::warn!(level = ?message.level, message = %message.text, "ACP PostToolUse hook");
            }
            for context in outcome.additional_context {
                if !context.trim().is_empty() {
                    report.llm_response.push_str("\nHook context: ");
                    report.llm_response.push_str(&context);
                }
            }
            if let Some(reason) = outcome.block_reason {
                report.llm_response.push_str("\nHook feedback: ");
                report.llm_response.push_str(&reason);
            }
        }

        let update = acp::ToolCallUpdate::new(call_id, Self::update_fields_from_report(&report));
        self.send_update(session_id, acp::SessionUpdate::ToolCallUpdate(update)).await?;

        Ok(Self::tool_call_result_from_report(call, report))
    }

    async fn run_permission_hook(
        &self,
        session: &SessionHandle,
        tool_name: &str,
        descriptor: ToolDescriptor,
        args: &Value,
    ) -> Result<Option<vtcode_core::hooks::PermissionRequestHookDecision>, SdkError> {
        let Some(hooks) = session.lifecycle_hooks() else {
            return Ok(None);
        };
        let request = build_permission_request(&self.config.workspace, &self.config.workspace, tool_name, Some(args));
        let suggestions = match descriptor {
            ToolDescriptor::Acp(tool) => self
                .permission_prompter
                .permission_options(tool, Some(args))
                .into_iter()
                .filter_map(|option| serde_json::to_value(option).ok())
                .collect::<Vec<_>>(),
            ToolDescriptor::Local => Vec::new(),
        };
        let outcome = hooks
            .run_permission_request(tool_name, Some(args), &request, &suggestions)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            tracing::warn!(level = ?message.level, message = %message.text, "ACP PermissionRequest hook");
        }
        Ok(outcome.decision)
    }

    async fn pace_tool_call(&self, session: &SessionHandle) {
        let Some(delay) = self.tool_call_delay else {
            return;
        };

        let sleep_for = {
            let Ok(data) = session.data.lock() else {
                return;
            };
            data.last_tool_call_at.and_then(|last_call| {
                let elapsed = last_call.elapsed();
                delay.checked_sub(elapsed)
            })
        };

        if let Some(duration) = sleep_for {
            tokio::time::sleep(duration).await;
        }

        if let Ok(mut data) = session.data.lock() {
            data.last_tool_call_at = Some(Instant::now());
        }
    }

    fn tool_name_from_call(call: &ProviderToolCall) -> &str {
        call.function
            .as_ref()
            .map(|function| function.name.as_str())
            .unwrap_or("unknown")
    }

    fn initial_tool_content(tool_name: &str, args: Option<&Value>) -> Vec<acp::ToolCallContent> {
        if tool_name != tools::APPLY_PATCH {
            return Vec::new();
        }

        args.and_then(|args| decode_apply_patch_input(args).ok().flatten())
            .map(|patch| vec![acp::ToolCallContent::from(patch.text)])
            .unwrap_or_default()
    }

    async fn capture_apply_patch_snapshot(&self, args: Option<&Value>) -> Option<ApplyPatchSnapshot> {
        let args = args?;
        let decoded = decode_apply_patch_input(args).ok().flatten()?;
        let patch = Patch::parse(&decoded.text).ok()?;
        let workspace = self.absolute_workspace_root().await?;
        capture_apply_patch_snapshot_for_workspace(&workspace, &patch).await
    }

    async fn absolute_workspace_root(&self) -> Option<PathBuf> {
        if self.workspace_root().is_absolute() {
            Some(self.workspace_root().to_path_buf())
        } else {
            canonicalize_with_context_async(self.workspace_root(), "ACP workspace")
                .await
                .ok()
        }
    }

    fn tool_call_result_from_report(call: &ProviderToolCall, report: ToolExecutionReport) -> ToolCallResult {
        ToolCallResult {
            tool_call_id: call.id.clone(),
            llm_response: report.llm_response,
            audit_status: report.audit_status,
        }
    }

    fn update_fields_from_report(report: &ToolExecutionReport) -> acp::ToolCallUpdateFields {
        let mut fields = acp::ToolCallUpdateFields::default().status(report.status);
        if !report.content.is_empty() {
            fields = fields.content(report.content.clone());
        }
        if !report.locations.is_empty() {
            fields = fields.locations(report.locations.clone());
        }
        if let Some(raw_output) = &report.raw_output {
            fields = fields.raw_output(raw_output.clone());
        }
        fields
    }

    async fn execute_descriptor(
        &self,
        descriptor: ToolDescriptor,
        tool_name: &str,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call_id: &str,
        args: &Value,
    ) -> ToolExecutionReport {
        if should_route_terminal_via_client(tool_name, args)
            && let Some(report) = self.execute_terminal_via_client(tool_name, client, session_id, args).await
        {
            return report;
        }

        match descriptor {
            ToolDescriptor::Acp(tool) => self.execute_acp_tool(tool, client, session_id, args).await,
            ToolDescriptor::Local => self.execute_local_tool(tool_name, args, call_id).await,
        }
    }

    async fn request_tool_permission(
        &self,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        call: &acp::ToolCall,
        descriptor: ToolDescriptor,
        tool: PermissionToolContext<'_>,
        args: &Value,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        match descriptor {
            ToolDescriptor::Acp(tool) => {
                self.permission_prompter
                    .request_tool_permission(client, session_id, call, tool, args)
                    .await
            }
            ToolDescriptor::Local => {
                self.permission_prompter
                    .request_named_tool_permission(client, session_id, call, tool, args)
                    .await
            }
        }
    }

    async fn execute_terminal_via_client(
        &self,
        tool_name: &str,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        args: &Value,
    ) -> Option<ToolExecutionReport> {
        if !self.client_supports_terminal() {
            return None;
        }

        match Self::requested_terminal_mode(args) {
            Ok(RunTerminalMode::Terminal) => None,
            Ok(RunTerminalMode::Pty) => {
                Some(match self.launch_client_terminal(tool_name, client, session_id, args).await {
                    Ok(report) => report,
                    Err(message) => ToolExecutionReport::failure(tool_name, &message),
                })
            }
            Err(message) => Some(ToolExecutionReport::failure(tool_name, &message)),
        }
    }

    async fn launch_client_terminal(
        &self,
        tool_name: &str,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        args: &Value,
    ) -> Result<ToolExecutionReport, String> {
        let command_parts = Self::parse_terminal_command(args)?;
        let (program, rest) = command_parts
            .split_first()
            .ok_or_else(|| "command array cannot be empty".to_string())?;

        let working_dir = self.resolve_terminal_working_dir(args)?;
        let location_display = self.describe_terminal_location(working_dir.as_ref());
        let command_display = command_parts.join(" ");

        let request = acp::CreateTerminalRequest::new(session_id.clone(), program.to_string())
            .args(rest.to_vec())
            .cwd(working_dir.clone());

        let response = client
            .create_terminal(request)
            .await
            .map_err(|error| format!("Failed to create terminal: {error}"))?;
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
        tool: SupportedTool,
        client: &ConnectionHandle,
        session_id: &acp::SessionId,
        args: &Value,
    ) -> ToolExecutionReport {
        match tool {
            SupportedTool::ReadFile => self
                .run_read_file(client, session_id, args)
                .await
                .unwrap_or_else(|message| ToolExecutionReport::failure(tools::READ_FILE, &message)),
            SupportedTool::ListFiles => self
                .run_list_files(args)
                .await
                .unwrap_or_else(|message| ToolExecutionReport::failure(tools::LIST_FILES, &message)),
        }
    }
}

struct ApplyPatchSnapshot {
    files: Vec<ApplyPatchFileSnapshot>,
}

struct ApplyPatchFileSnapshot {
    path: PathBuf,
    old_text: Option<String>,
    new_text: ApplyPatchNewText,
}

enum ApplyPatchNewText {
    Empty,
    ReadAfterWrite,
}

async fn capture_apply_patch_snapshot_for_workspace(
    workspace: &std::path::Path,
    patch: &Patch,
) -> Option<ApplyPatchSnapshot> {
    let mut files = Vec::new();

    for operation in patch.operations() {
        match operation {
            PatchOperation::AddFile { path, .. } => files.push(ApplyPatchFileSnapshot {
                path: workspace.join(path),
                old_text: None,
                new_text: ApplyPatchNewText::ReadAfterWrite,
            }),
            PatchOperation::DeleteFile { path } => files.push(ApplyPatchFileSnapshot {
                path: workspace.join(path),
                old_text: Some(tokio::fs::read_to_string(workspace.join(path)).await.ok()?),
                new_text: ApplyPatchNewText::Empty,
            }),
            PatchOperation::UpdateFile { path, new_path, .. } => {
                let source_path = workspace.join(path);
                let old_text = tokio::fs::read_to_string(&source_path).await.ok()?;
                let destination_path = new_path
                    .as_deref()
                    .filter(|candidate| *candidate != path.as_str())
                    .map(|destination| workspace.join(destination));

                files.push(ApplyPatchFileSnapshot {
                    path: source_path,
                    old_text: Some(old_text),
                    new_text: if destination_path.is_some() {
                        ApplyPatchNewText::Empty
                    } else {
                        ApplyPatchNewText::ReadAfterWrite
                    },
                });
                if let Some(destination_path) = destination_path {
                    files.push(ApplyPatchFileSnapshot {
                        path: destination_path,
                        old_text: None,
                        new_text: ApplyPatchNewText::ReadAfterWrite,
                    });
                }
            }
        }
    }

    Some(ApplyPatchSnapshot { files })
}

async fn attach_apply_patch_diff_content(report: &mut ToolExecutionReport, snapshot: Option<&ApplyPatchSnapshot>) {
    if !matches!(report.status, acp::ToolCallStatus::Completed) {
        return;
    }
    let Some(snapshot) = snapshot else {
        return;
    };
    let Some(content) = render_apply_patch_diff_content(snapshot).await else {
        return;
    };
    report.content = content;
}

async fn render_apply_patch_diff_content(snapshot: &ApplyPatchSnapshot) -> Option<Vec<acp::ToolCallContent>> {
    let mut content = Vec::with_capacity(snapshot.files.len());
    for file in &snapshot.files {
        let new_text = match file.new_text {
            ApplyPatchNewText::Empty => String::new(),
            ApplyPatchNewText::ReadAfterWrite => tokio::fs::read_to_string(&file.path).await.ok()?,
        };
        content.push(create_diff_content(file.path.to_string_lossy().as_ref(), file.old_text.as_deref(), &new_text));
    }
    Some(content)
}

fn should_route_terminal_via_client(tool_name: &str, _args: &Value) -> bool {
    matches!(tool_name, tools::RUN_PTY_CMD | tools::EXEC_COMMAND)
}

fn should_run_post_tool_hook(status: &acp::ToolCallStatus) -> bool {
    matches!(status, acp::ToolCallStatus::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_fs::TempDir;
    use std::fs;

    #[test]
    fn apply_patch_call_exposes_patch_text_before_execution() {
        let patch = "*** Begin Patch\n*** Add File: visible.txt\n+hello\n*** End Patch\n";
        let content = ZedAgent::initial_tool_content(tools::APPLY_PATCH, Some(&json!({ "patch": patch })));

        let [acp::ToolCallContent::Content(content)] = content.as_slice() else {
            panic!("apply_patch should expose one text content block");
        };
        let acp::ContentBlock::Text(text) = &content.content else {
            panic!("apply_patch preview should be text");
        };
        assert_eq!(text.text, patch);
    }

    #[test]
    fn other_tool_calls_do_not_echo_raw_arguments_as_content() {
        assert!(ZedAgent::initial_tool_content(tools::EXEC_COMMAND, Some(&json!({ "command": "true" }))).is_empty());
    }

    #[test]
    fn post_tool_hook_only_runs_for_successful_completion() {
        assert!(should_run_post_tool_hook(&acp::ToolCallStatus::Completed));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::Failed));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::InProgress));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::Pending));
    }

    #[tokio::test]
    async fn apply_patch_diff_content_covers_update_add_delete_move_and_multi_file() {
        let temp = TempDir::new().unwrap();
        fs::write(temp.path().join("update.txt"), "old update").unwrap();
        fs::write(temp.path().join("delete.txt"), "old delete").unwrap();
        fs::write(temp.path().join("move.txt"), "old move").unwrap();
        let patch = Patch::parse(
            "*** Begin Patch\n*** Update File: update.txt\n@@\n-old update\n+new update\n*** Add File: add.txt\n+new add\n*** Delete File: delete.txt\n*** Update File: move.txt\n*** Move to: moved.txt\n@@\n-old move\n+new move\n*** End Patch\n",
        )
        .unwrap();

        let snapshot = capture_apply_patch_snapshot_for_workspace(temp.path(), &patch).await.unwrap();
        assert_eq!(snapshot.files.len(), 5);
        assert!(snapshot.files.iter().all(|file| file.path.is_absolute()));
        assert_eq!(snapshot.files[0].old_text.as_deref(), Some("old update"));
        assert_eq!(snapshot.files[1].old_text, None);
        assert_eq!(snapshot.files[2].old_text.as_deref(), Some("old delete"));
        assert_eq!(snapshot.files[3].old_text.as_deref(), Some("old move"));
        assert_eq!(snapshot.files[4].old_text, None);

        fs::write(temp.path().join("update.txt"), "new update").unwrap();
        fs::write(temp.path().join("add.txt"), "new add").unwrap();
        fs::remove_file(temp.path().join("delete.txt")).unwrap();
        fs::remove_file(temp.path().join("move.txt")).unwrap();
        fs::write(temp.path().join("moved.txt"), "new move").unwrap();

        let content = render_apply_patch_diff_content(&snapshot).await.unwrap();
        assert_eq!(content.len(), 5);
        let diffs = content
            .iter()
            .map(|block| {
                let acp::ToolCallContent::Diff(diff) = block else {
                    panic!("expected ACP diff content");
                };
                diff
            })
            .collect::<Vec<_>>();
        assert_eq!(diffs[0].old_text.as_deref(), Some("old update"));
        assert_eq!(diffs[0].new_text, "new update");
        assert_eq!(diffs[1].old_text, None);
        assert_eq!(diffs[1].new_text, "new add");
        assert_eq!(diffs[2].old_text.as_deref(), Some("old delete"));
        assert_eq!(diffs[2].new_text, "");
        assert_eq!(diffs[3].old_text.as_deref(), Some("old move"));
        assert_eq!(diffs[3].new_text, "");
        assert_eq!(diffs[4].old_text, None);
        assert_eq!(diffs[4].new_text, "new move");
        assert_eq!(diffs[0].path, temp.path().join("update.txt"));
        assert_eq!(diffs[4].path, temp.path().join("moved.txt"));
    }

    #[tokio::test]
    async fn failed_or_cancelled_apply_patch_does_not_attach_diff_content() {
        let snapshot = ApplyPatchSnapshot { files: Vec::new() };

        let mut failed = ToolExecutionReport::failure(tools::APPLY_PATCH, "failed");
        let failed_content = failed.content.clone();
        attach_apply_patch_diff_content(&mut failed, Some(&snapshot)).await;
        assert_eq!(failed.content, failed_content);

        let mut cancelled = ToolExecutionReport::cancelled(tools::APPLY_PATCH);
        let cancelled_content = cancelled.content.clone();
        attach_apply_patch_diff_content(&mut cancelled, Some(&snapshot)).await;
        assert_eq!(cancelled.content, cancelled_content);
    }
}
