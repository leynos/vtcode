//! Runs non-local ACP tools, including permission checks and protocol-facing result reporting.
use super::ZedAgent;
use crate::acp;
use crate::acp::Error as SdkError;
use crate::audit::timestamp_unix_ms;
use crate::permissions::{AcpPermissionPrompter, PermissionToolContext};
use crate::reports::{
    TOOL_RESPONSE_KEY_STATUS, TOOL_RESPONSE_KEY_TOOL, TOOL_SUCCESS_LABEL, ToolExecutionReport, create_diff_content,
};
use crate::tooling::{AcpToolRegistry, SupportedTool, ToolDescriptor};
use crate::zed::connection::ConnectionHandle;
use anyhow::Result;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant as StdInstant;
use tokio::time::Instant;
use vtcode_commons::fs::canonicalize_with_context_async;
use vtcode_core::config::constants::tools;
use vtcode_core::hooks::{PermissionDecisionBehaviour, PreToolHookDecision};
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;
use vtcode_core::permissions::build_permission_request;
use vtcode_core::tools::apply_patch::{Patch, PatchOperation, decode_apply_patch_input};
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

use super::super::types::{RunTerminalMode, SessionHandle, ToolCallResult};
use super::task_progress::task_plan_from_report;
use super::tool_execution_local::{ClientFileReadRequest, LocalToolInvocation, SessionLocalToolRequest};

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
                self.record_tool_audit(ToolAuditInput {
                    session,
                    session_id,
                    turn_id: &turn_id,
                    call,
                    effective_arguments: &result.audit_arguments,
                    result: Ok((&result.llm_response, result.audit_status)),
                    duration_ms: 0,
                })
                .await;
                results.push(result);
            }
            return Ok(results);
        };

        let mut results = Vec::with_capacity(calls.len());

        for call in calls {
            self.pace_tool_call(session).await?;
            results.push(
                self.execute_tool_call(ToolCallInvocation {
                    session,
                    session_id,
                    turn_id: &turn_id,
                    call,
                    client: client.as_ref(),
                })
                .await?,
            );
        }

        Ok(results)
    }

    async fn execute_tool_call(&self, invocation: ToolCallInvocation<'_>) -> Result<ToolCallResult, SdkError> {
        let started = StdInstant::now();
        let mut audit_arguments = Self::raw_tool_arguments(invocation.call);
        let result = Box::pin(self.execute_tool_call_inner(invocation, &mut audit_arguments)).await;
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.record_tool_audit(ToolAuditInput {
            session: invocation.session,
            session_id: invocation.session_id,
            turn_id: invocation.turn_id,
            call: invocation.call,
            effective_arguments: &audit_arguments,
            result: match &result {
                Ok(result) => Ok((&result.llm_response, result.audit_status)),
                Err(_) => Err("ACP tool execution failed"),
            },
            duration_ms,
        })
        .await;
        result
    }

    pub(super) async fn record_tool_audit(&self, input: ToolAuditInput<'_>) {
        let Some(logger) = self.audit_logger.as_ref() else {
            return;
        };
        let tool_name = Self::tool_name_from_call(input.call);
        let model_id = match input.session.data.lock() {
            Ok(data) => data.model.clone(),
            Err(error) => {
                tracing::warn!(%error, tool = tool_name, "Failed to read ACP session model for audit entry");
                self.config.model.clone()
            }
        };
        let (status, reason, result_bytes) = match input.result {
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
                session_id: &input.session_id.to_string(),
                turn_id: input.turn_id,
                tool_call_id: &input.call.id,
                tool_name,
                arguments: input.effective_arguments,
                result: &result_bytes,
                duration_ms: input.duration_ms,
                status,
                model_id: Some(&model_id),
                reason: reason.as_deref(),
            });
        if let Err(error) = logger.record(entry).await {
            tracing::warn!(%error, tool = tool_name, call_id = %input.call.id, "Failed to write ACP tool audit entry");
        }
    }

    async fn execute_tool_call_inner(
        &self,
        invocation: ToolCallInvocation<'_>,
        audit_arguments: &mut Vec<u8>,
    ) -> Result<ToolCallResult, SdkError> {
        let workspace_runtime = invocation.session.workspace_runtime().map_err(|error| {
            SdkError::internal_error()
                .data(format!("Failed to read workspace runtime for ACP session {}: {error:#}", invocation.session_id))
        })?;
        let acp_tool_registry = workspace_runtime
            .as_ref()
            .map_or(self.acp_tool_registry.as_ref(), |runtime| runtime.acp_tool_registry.as_ref());
        let permission_prompter = workspace_runtime
            .as_ref()
            .map_or(self.permission_prompter.as_ref(), |runtime| runtime.permission_prompter.as_ref());
        let local_tool_registry = workspace_runtime
            .as_ref()
            .map_or(&self.local_tool_registry, |runtime| &runtime.local_tool_registry);
        let Some(func_ref) = invocation.call.function.as_ref() else {
            return Ok(Self::tool_call_result_from_report(
                invocation.call,
                ToolExecutionReport::failure("unknown", "Malformed tool call: missing function payload"),
            ));
        };
        let context = ToolExecutionContext {
            invocation,
            acp_tool_registry,
            permission_prompter,
            local_tool_registry,
        };
        let resolved = Self::resolve_tool_call(invocation.call.id.as_str(), func_ref, context.acp_tool_registry);
        let pre_tool_decision = self.run_pre_tool_stage(&context, &resolved).await?;

        if matches!(pre_tool_decision, PreToolHookDecision::Deny) {
            let report = ToolExecutionReport::blocked(resolved.tool_name, "Tool execution denied by PreToolUse hook");
            let update = acp::ToolCallUpdate::new(resolved.call_id.clone(), Self::update_fields_from_report(&report));
            self.send_update(invocation.session_id, acp::SessionUpdate::ToolCallUpdate(update))
                .await?;
            return Ok(Self::tool_call_result_from_report(invocation.call, report));
        }

        let authorization = self
            .authorize_tool_call(&context, &resolved, ToolAuthorizationInput { pre_tool_decision, audit_arguments })
            .await?;
        let execution = self.execute_authorized_tool(&context, &resolved, authorization).await?;
        self.finish_tool_call(&context, &resolved, execution).await
    }

    fn resolve_tool_call<'a>(
        call_id: &str,
        function: &'a vtcode_commons::llm::FunctionCall,
        registry: &AcpToolRegistry,
    ) -> ResolvedToolCall<'a> {
        let descriptor = registry.lookup(&function.name);
        let args_parse_result: Result<Value, _> = serde_json::from_str(&function.arguments);
        let parsed_args = args_parse_result.as_ref().ok().cloned();
        let title = match (descriptor, parsed_args.as_ref()) {
            (Some(descriptor), Some(args)) => registry.render_title(descriptor, &function.name, args),
            (Some(descriptor), None) => registry.render_title(descriptor, &function.name, &Value::Null),
            (None, _) => format!("{} (unsupported)", function.name),
        };
        let acp_call_id = acp::ToolCallId::new(Arc::from(call_id.to_owned()));
        let kind = match descriptor {
            Some(ToolDescriptor::Acp(tool)) => tool.kind(),
            Some(ToolDescriptor::Local) | None => registry.tool_kind_for_call(&function.name, parsed_args.as_ref()),
        };
        let initial_call = acp::ToolCall::new(acp_call_id.clone(), title)
            .kind(kind)
            .status(acp::ToolCallStatus::Pending)
            .content(Self::initial_tool_content(&function.name, parsed_args.as_ref()))
            .raw_input(parsed_args.clone());
        ResolvedToolCall {
            tool_name: &function.name,
            raw_arguments: &function.arguments,
            descriptor,
            parsed_args,
            args_parse_error: args_parse_result.err().map(|error| error.to_string()),
            call_id: acp_call_id,
            kind,
            initial_call,
        }
    }

    async fn run_pre_tool_stage(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
    ) -> Result<PreToolHookDecision, SdkError> {
        let decision = if let Some(hooks) = context.invocation.session.lifecycle_hooks().map_err(|error| {
            SdkError::internal_error().data(format!(
                "Failed to read lifecycle hooks for ACP session {}: {error:#}",
                context.invocation.session_id
            ))
        })? {
            let outcome = hooks
                .run_pre_tool_use(
                    resolved.tool_name,
                    resolved.parsed_args.as_ref(),
                    Some(context.invocation.call.id.as_str()),
                )
                .await
                .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
            for message in outcome.messages {
                tracing::warn!(level = ?message.level, message = %message.text, "ACP PreToolUse hook");
            }
            outcome.decision
        } else {
            PreToolHookDecision::Continue
        };
        self.send_update(context.invocation.session_id, acp::SessionUpdate::ToolCall(resolved.initial_call.clone()))
            .await?;
        Ok(decision)
    }

    async fn authorize_tool_call(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        input: ToolAuthorizationInput<'_>,
    ) -> Result<ToolAuthorization, SdkError> {
        let was_cancelled_before_permission = context.invocation.session.cancellation.is_cancelled();
        let mut authorization = ToolAuthorization::new(resolved);
        let permission_hook_decision = self
            .permission_hook_decision(context, resolved, was_cancelled_before_permission)
            .await?;
        *input.audit_arguments = authorization.audit_arguments.clone();
        let permission_override = self
            .permission_override(
                context,
                resolved,
                PermissionResolutionInput {
                    pre_tool_decision: input.pre_tool_decision,
                    hook_decision: permission_hook_decision,
                    was_cancelled_before_permission,
                    authorization: &mut authorization,
                },
            )
            .await?;
        authorization.permission_override = permission_override;

        authorization.cancel_after_permission = context.invocation.session.cancellation.is_cancelled();
        *input.audit_arguments = authorization.audit_arguments.clone();
        self.mark_local_tool_preapproved(context, resolved, &authorization).await;
        self.send_tool_call_in_progress(context, resolved, &authorization).await?;
        Ok(authorization)
    }

    async fn permission_override(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        input: PermissionResolutionInput<'_>,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        if let Some(decision) = input.hook_decision {
            return Ok(input.authorization.apply_permission_hook_decision(
                resolved.tool_name,
                resolved.raw_arguments,
                decision,
            ));
        }
        if matches!(input.pre_tool_decision, PreToolHookDecision::Allow) || input.was_cancelled_before_permission {
            return Ok(None);
        }
        let (Some(descriptor), Some(args)) = (resolved.descriptor, input.authorization.effective_args.as_ref()) else {
            return Ok(None);
        };
        self.request_tool_permission_for_call(
            context,
            resolved,
            ToolPermissionPrompt {
                descriptor,
                args,
                force_prompt: matches!(input.pre_tool_decision, PreToolHookDecision::Ask),
            },
        )
        .await
    }

    async fn permission_hook_decision(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        was_cancelled_before_permission: bool,
    ) -> Result<Option<vtcode_core::hooks::PermissionRequestHookDecision>, SdkError> {
        if was_cancelled_before_permission {
            return Ok(None);
        }
        if resolved.descriptor.is_none() {
            return Ok(None);
        }
        let Some(args) = resolved.parsed_args.as_ref() else {
            return Ok(None);
        };
        self.run_permission_hook(context, resolved, args).await
    }

    async fn mark_local_tool_preapproved(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        authorization: &ToolAuthorization,
    ) {
        let is_authorized_local_call = matches!(resolved.descriptor, Some(ToolDescriptor::Local))
            && authorization.effective_args.is_some()
            && authorization.permission_override.is_none()
            && !authorization.cancel_after_permission;
        if is_authorized_local_call {
            // ACP has already completed (or explicitly bypassed) its permission
            // flow. Authorize exactly this execution in the core registry so a
            // local `prompt` policy does not fail closed a second time.
            context.local_tool_registry.mark_tool_preapproved(resolved.tool_name).await;
        }
    }

    async fn send_tool_call_in_progress(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        authorization: &ToolAuthorization,
    ) -> Result<(), SdkError> {
        if resolved.descriptor.is_none() {
            return Ok(());
        }
        if authorization.permission_override.is_some() {
            return Ok(());
        }
        if authorization.cancel_after_permission {
            return Ok(());
        }
        let fields = acp::ToolCallUpdateFields::default().status(acp::ToolCallStatus::InProgress);
        let update = acp::ToolCallUpdate::new(resolved.call_id.clone(), fields);
        self.send_update(context.invocation.session_id, acp::SessionUpdate::ToolCallUpdate(update))
            .await
    }

    async fn request_tool_permission_for_call(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        request: ToolPermissionPrompt<'_>,
    ) -> Result<Option<ToolExecutionReport>, SdkError> {
        match (request.descriptor, request.force_prompt) {
            (ToolDescriptor::Acp(tool), true) => {
                context
                    .permission_prompter
                    .request_tool_permission_forced(
                        context.invocation.client,
                        context.invocation.session_id,
                        &resolved.initial_call,
                        tool,
                        request.args,
                    )
                    .await
            }
            (ToolDescriptor::Acp(tool), false) => {
                context
                    .permission_prompter
                    .request_tool_permission(
                        context.invocation.client,
                        context.invocation.session_id,
                        &resolved.initial_call,
                        tool,
                        request.args,
                    )
                    .await
            }
            (ToolDescriptor::Local, true) => {
                context
                    .permission_prompter
                    .request_named_tool_permission_forced(
                        context.invocation.client,
                        context.invocation.session_id,
                        &resolved.initial_call,
                        PermissionToolContext::new(
                            resolved.tool_name,
                            resolved.kind,
                            resolved.initial_call.title.as_str(),
                        ),
                        request.args,
                    )
                    .await
            }
            (ToolDescriptor::Local, false) => {
                context
                    .permission_prompter
                    .request_named_tool_permission(
                        context.invocation.client,
                        context.invocation.session_id,
                        &resolved.initial_call,
                        PermissionToolContext::new(
                            resolved.tool_name,
                            resolved.kind,
                            resolved.initial_call.title.as_str(),
                        ),
                        request.args,
                    )
                    .await
            }
        }
    }

    async fn execute_authorized_tool(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        authorization: ToolAuthorization,
    ) -> Result<AuthorizedToolExecution, SdkError> {
        let ToolAuthorization {
            effective_args,
            permission_override,
            cancel_after_permission,
            audit_arguments,
        } = authorization;
        let apply_patch_snapshot = if should_capture_apply_patch_snapshot(
            resolved.tool_name,
            permission_override.is_some(),
            cancel_after_permission,
        ) {
            self.capture_apply_patch_snapshot(context.invocation.session, effective_args.as_ref())
                .await?
        } else {
            None
        };

        let mut report = match (permission_override, cancel_after_permission) {
            (Some(report), _) => report,
            (None, true) => ToolExecutionReport::cancelled(resolved.tool_name),
            (None, false) => self.execute_resolved_tool(context, resolved, effective_args.as_ref()).await,
        };
        if context.invocation.session.cancellation.is_cancelled()
            && matches!(report.status, acp::ToolCallStatus::Completed)
        {
            report = ToolExecutionReport::cancelled(resolved.tool_name);
        }
        attach_apply_patch_diff_content(&mut report, apply_patch_snapshot.as_ref()).await;
        Ok(AuthorizedToolExecution { report, effective_args, audit_arguments })
    }

    async fn execute_resolved_tool(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        args: Option<&Value>,
    ) -> ToolExecutionReport {
        match (resolved.descriptor, args) {
            (Some(descriptor), Some(args)) => self.execute_descriptor(context, descriptor, args).await,
            (None, Some(_)) => ToolExecutionReport::failure(resolved.tool_name, "Unsupported tool"),
            (Some(_), None) => {
                let error = resolved.args_parse_error.as_ref().map_or_else(
                    || "Invalid JSON arguments".to_owned(),
                    |error| format!("Invalid JSON arguments: {error}"),
                );
                ToolExecutionReport::failure(resolved.tool_name, &error)
            }
            (None, None) => ToolExecutionReport::failure(resolved.tool_name, "Invalid JSON arguments"),
        }
    }

    async fn finish_tool_call(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        mut execution: AuthorizedToolExecution,
    ) -> Result<ToolCallResult, SdkError> {
        self.run_post_tool_stage(context, resolved, &mut execution).await?;

        let update =
            acp::ToolCallUpdate::new(resolved.call_id.clone(), Self::update_fields_from_report(&execution.report));
        self.send_update(context.invocation.session_id, acp::SessionUpdate::ToolCallUpdate(update))
            .await?;
        if resolved.tool_name == tools::TASK_TRACKER
            && let Some(plan) = task_plan_from_report(&execution.report)
        {
            self.send_update(context.invocation.session_id, acp::SessionUpdate::Plan(plan))
                .await?;
        }

        let mut result = Self::tool_call_result_from_report(context.invocation.call, execution.report);
        result.audit_arguments = execution.audit_arguments;
        Ok(result)
    }

    async fn run_post_tool_stage(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        execution: &mut AuthorizedToolExecution,
    ) -> Result<(), SdkError> {
        if !should_run_post_tool_hook(&execution.report.status) {
            return Ok(());
        }
        let Some(hooks) = context.invocation.session.lifecycle_hooks().map_err(|error| {
            SdkError::internal_error().data(format!(
                "Failed to read lifecycle hooks for ACP session {}: {error:#}",
                context.invocation.session_id
            ))
        })?
        else {
            return Ok(());
        };
        let output = execution
            .report
            .raw_output
            .as_ref()
            .cloned()
            .unwrap_or_else(|| Value::String(execution.report.llm_response.clone()));
        let outcome = hooks
            .run_post_tool_use(
                resolved.tool_name,
                execution.effective_args.as_ref(),
                &output,
                Some(context.invocation.call.id.as_str()),
            )
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            tracing::warn!(level = ?message.level, message = %message.text, "ACP PostToolUse hook");
        }
        for context in outcome.additional_context {
            if !context.trim().is_empty() {
                execution.report.llm_response.push_str("\nHook context: ");
                execution.report.llm_response.push_str(&context);
            }
        }
        if let Some(reason) = outcome.block_reason {
            execution.report.llm_response.push_str("\nHook feedback: ");
            execution.report.llm_response.push_str(&reason);
        }
        Ok(())
    }

    async fn run_permission_hook(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        args: &Value,
    ) -> Result<Option<vtcode_core::hooks::PermissionRequestHookDecision>, SdkError> {
        let Some(hooks) = context.invocation.session.lifecycle_hooks().map_err(|error| {
            SdkError::internal_error()
                .data(format!("Failed to read lifecycle hooks for ACP permission hook: {error:#}"))
        })?
        else {
            return Ok(None);
        };
        let workspace = context
            .invocation
            .session
            .workspace_runtime()
            .map_err(|error| {
                SdkError::internal_error()
                    .data(format!("Failed to read workspace runtime for ACP permission hook: {error:#}"))
            })?
            .map_or_else(|| self.config.workspace.clone(), |runtime| runtime.workspace_root.clone());
        let request = build_permission_request(&workspace, &workspace, resolved.tool_name, Some(args));
        let Some(descriptor) = resolved.descriptor else {
            return Ok(None);
        };
        let suggestions = match descriptor {
            ToolDescriptor::Acp(tool) => context
                .permission_prompter
                .permission_options(tool, Some(args))
                .into_iter()
                .filter_map(|option| serde_json::to_value(option).ok())
                .collect::<Vec<_>>(),
            ToolDescriptor::Local => Vec::new(),
        };
        let outcome = hooks
            .run_permission_request(resolved.tool_name, Some(args), &request, &suggestions)
            .await
            .map_err(|error| SdkError::internal_error().data(error.to_string()))?;
        for message in outcome.messages {
            tracing::warn!(level = ?message.level, message = %message.text, "ACP PermissionRequest hook");
        }
        Ok(outcome.decision)
    }

    async fn pace_tool_call(&self, session: &SessionHandle) -> Result<(), SdkError> {
        let Some(delay) = self.tool_call_delay else {
            return Ok(());
        };

        let sleep_for = {
            let data = session.data.lock().map_err(|error| {
                SdkError::internal_error().data(format!("Failed to read ACP session pacing state: {error}"))
            })?;
            data.last_tool_call_at.and_then(|last_call| {
                let elapsed = last_call.elapsed();
                delay.checked_sub(elapsed)
            })
        };

        if let Some(duration) = sleep_for {
            tokio::time::sleep(duration).await;
        }

        let mut data = session.data.lock().map_err(|error| {
            SdkError::internal_error().data(format!("Failed to update ACP session pacing state: {error}"))
        })?;
        data.last_tool_call_at = Some(Instant::now());
        Ok(())
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

    async fn capture_apply_patch_snapshot(
        &self,
        session: &SessionHandle,
        args: Option<&Value>,
    ) -> Result<Option<ApplyPatchSnapshot>, SdkError> {
        let Some(args) = args else {
            return Ok(None);
        };
        let Some(decoded) = decode_apply_patch_input(args).ok().flatten() else {
            return Ok(None);
        };
        let Ok(patch) = Patch::parse(&decoded.text) else {
            return Ok(None);
        };
        let Some(workspace) = self.absolute_workspace_root(session).await? else {
            return Ok(None);
        };
        Ok(capture_apply_patch_snapshot_for_workspace(&workspace, &patch).await)
    }

    async fn absolute_workspace_root(&self, session: &SessionHandle) -> Result<Option<PathBuf>, SdkError> {
        let workspace = session
            .workspace_runtime()
            .map_err(|error| {
                SdkError::internal_error().data(format!("Failed to read ACP session workspace runtime: {error:#}"))
            })?
            .map_or_else(|| self.workspace_root().to_path_buf(), |runtime| runtime.workspace_root.clone());
        if workspace.is_absolute() {
            Ok(Some(workspace))
        } else {
            Ok(canonicalize_with_context_async(&workspace, "ACP workspace").await.ok())
        }
    }

    fn tool_call_result_from_report(call: &ProviderToolCall, report: ToolExecutionReport) -> ToolCallResult {
        ToolCallResult {
            tool_call_id: call.id.clone(),
            llm_response: report.llm_response,
            audit_status: report.audit_status,
            audit_arguments: Self::raw_tool_arguments(call),
        }
    }

    fn raw_tool_arguments(call: &ProviderToolCall) -> Vec<u8> {
        call.function
            .as_ref()
            .map_or_else(Vec::new, |function| function.arguments.as_bytes().to_vec())
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

        let response = context
            .invocation
            .client
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

#[derive(Clone, Copy)]
struct ToolCallInvocation<'a> {
    session: &'a SessionHandle,
    session_id: &'a acp::SessionId,
    turn_id: &'a str,
    call: &'a ProviderToolCall,
    client: &'a ConnectionHandle,
}

pub(super) struct ToolAuditInput<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) session_id: &'a acp::SessionId,
    pub(super) turn_id: &'a str,
    pub(super) call: &'a ProviderToolCall,
    pub(super) effective_arguments: &'a [u8],
    pub(super) result: Result<(&'a str, vtcode_safety::audit_log::ToolAuditStatus), &'a str>,
    pub(super) duration_ms: u64,
}

struct ToolExecutionContext<'a> {
    invocation: ToolCallInvocation<'a>,
    acp_tool_registry: &'a AcpToolRegistry,
    permission_prompter: &'a dyn AcpPermissionPrompter,
    local_tool_registry: &'a CoreToolRegistry,
}

struct ToolAuthorizationInput<'a> {
    pre_tool_decision: PreToolHookDecision,
    audit_arguments: &'a mut Vec<u8>,
}

struct PermissionResolutionInput<'a> {
    pre_tool_decision: PreToolHookDecision,
    hook_decision: Option<vtcode_core::hooks::PermissionRequestHookDecision>,
    was_cancelled_before_permission: bool,
    authorization: &'a mut ToolAuthorization,
}

struct ToolPermissionPrompt<'a> {
    descriptor: ToolDescriptor,
    args: &'a Value,
    force_prompt: bool,
}

struct ResolvedToolCall<'a> {
    tool_name: &'a str,
    raw_arguments: &'a str,
    descriptor: Option<ToolDescriptor>,
    parsed_args: Option<Value>,
    args_parse_error: Option<String>,
    call_id: acp::ToolCallId,
    kind: acp::ToolKind,
    initial_call: acp::ToolCall,
}

struct ToolAuthorization {
    effective_args: Option<Value>,
    audit_arguments: Vec<u8>,
    permission_override: Option<ToolExecutionReport>,
    cancel_after_permission: bool,
}

impl ToolAuthorization {
    fn new(resolved: &ResolvedToolCall<'_>) -> Self {
        let effective_args = resolved.parsed_args.clone();
        let audit_arguments = effective_tool_arguments(resolved.raw_arguments, effective_args.as_ref());
        Self {
            effective_args,
            audit_arguments,
            permission_override: None,
            cancel_after_permission: false,
        }
    }

    fn apply_permission_hook_decision(
        &mut self,
        tool_name: &str,
        raw_arguments: &str,
        decision: vtcode_core::hooks::PermissionRequestHookDecision,
    ) -> Option<ToolExecutionReport> {
        if let Some(updated_input) = decision.updated_input {
            self.effective_args = Some(updated_input);
            self.audit_arguments = effective_tool_arguments(raw_arguments, self.effective_args.as_ref());
        }
        if decision.scope != vtcode_core::hooks::PermissionDecisionScope::Once {
            tracing::warn!(tool = %tool_name, scope = ?decision.scope, "ACP PermissionRequest hook scope is not persisted");
        }
        if !decision.permission_updates.is_empty() {
            tracing::warn!(tool = %tool_name, "ACP PermissionRequest hook permission updates are unsupported");
        }
        if decision.interrupt {
            tracing::warn!(tool = %tool_name, "ACP PermissionRequest hook interrupted tool execution");
            return Some(ToolExecutionReport::failure(
                tool_name,
                "Tool execution interrupted by PermissionRequest hook",
            ));
        }
        match decision.behaviour {
            PermissionDecisionBehaviour::Allow => None,
            PermissionDecisionBehaviour::Deny => {
                Some(ToolExecutionReport::blocked(tool_name, "Tool execution denied by PermissionRequest hook"))
            }
        }
    }
}

struct AuthorizedToolExecution {
    report: ToolExecutionReport,
    effective_args: Option<Value>,
    audit_arguments: Vec<u8>,
}

fn effective_tool_arguments(raw_arguments: &str, effective_args: Option<&Value>) -> Vec<u8> {
    effective_args
        .and_then(|arguments| serde_json::to_vec(arguments).ok())
        .unwrap_or_else(|| raw_arguments.as_bytes().to_vec())
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

fn should_capture_apply_patch_snapshot(tool_name: &str, permission_overridden: bool, cancelled: bool) -> bool {
    tool_name == tools::APPLY_PATCH && !permission_overridden && !cancelled
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

    #[test]
    fn audit_arguments_use_permission_hook_updated_input() {
        let raw_arguments = r#"{"path":"original.txt"}"#;
        let updated_arguments = json!({ "path": "approved.txt" });

        let audit_arguments = effective_tool_arguments(raw_arguments, Some(&updated_arguments));

        assert_eq!(
            serde_json::from_slice::<Value>(&audit_arguments).expect("effective audit arguments"),
            updated_arguments
        );
        assert_ne!(audit_arguments, raw_arguments.as_bytes());
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
