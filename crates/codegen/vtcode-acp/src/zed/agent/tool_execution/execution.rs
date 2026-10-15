//! Coordinate ACP tool execution, audit recording, and result finalization.
use super::super::ZedAgent;
use super::super::task_progress::task_plan_from_report;
use super::apply_patch::{
    ApplyPatchSnapshot, attach_apply_patch_diff_content, capture_apply_patch_snapshot_for_workspace,
};
use super::data::ResolvedToolCall;
use super::data::{
    AuthorizedToolExecution, ToolAuditInput, ToolAuthorization, ToolCallInvocation, ToolExecutionContext,
};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::audit::timestamp_unix_ms;
use crate::reports::ToolExecutionReport;
use crate::zed::types::{SessionHandle, ToolCallResult};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant as StdInstant;
use tokio::time::Instant;
use vtcode_commons::fs::canonicalize_with_context_async;
use vtcode_core::config::constants::tools;
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;
use vtcode_core::tools::apply_patch::{Patch, decode_apply_patch_input};

impl ZedAgent {
    pub(super) async fn execute_tool_call(
        &self,
        invocation: ToolCallInvocation<'_>,
    ) -> Result<ToolCallResult, SdkError> {
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

    pub(in crate::zed::agent) async fn record_tool_audit(&self, input: ToolAuditInput<'_>) {
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

    pub(super) async fn execute_authorized_tool(
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
        append_cancellation_after_completion_note(&mut report, context.invocation.session.cancellation.is_cancelled());
        attach_apply_patch_diff_content(&mut report, apply_patch_snapshot.as_ref()).await;
        Ok(AuthorizedToolExecution { report, effective_args, audit_arguments })
    }

    pub(super) async fn execute_resolved_tool(
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

    pub(super) async fn finish_tool_call(
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

    pub(super) async fn run_post_tool_stage(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        execution: &mut AuthorizedToolExecution,
    ) -> Result<(), SdkError> {
        if !should_run_post_tool_hook(&execution.report.status) {
            return Ok(());
        }
        let hooks = match context.invocation.session.lifecycle_hooks() {
            Ok(Some(hooks)) => hooks,
            Ok(None) => return Ok(()),
            Err(error) => {
                tracing::warn!(
                    %error,
                    session_id = %context.invocation.session_id,
                    tool = resolved.tool_name,
                    call_id = %resolved.call_id,
                    "Failed to read ACP PostToolUse hooks after tool completion; preserving the completed tool result"
                );
                append_post_tool_hook_failure_note(&mut execution.report);
                return Ok(());
            }
        };
        let output = execution
            .report
            .raw_output
            .as_ref()
            .cloned()
            .unwrap_or_else(|| Value::String(execution.report.llm_response.clone()));
        let outcome = match hooks
            .run_post_tool_use(
                resolved.tool_name,
                execution.effective_args.as_ref(),
                &output,
                Some(context.invocation.call.id.as_str()),
            )
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                tracing::warn!(
                    %error,
                    session_id = %context.invocation.session_id,
                    tool = resolved.tool_name,
                    call_id = %resolved.call_id,
                    "ACP PostToolUse hook failed after tool completion; preserving the completed tool result"
                );
                append_post_tool_hook_failure_note(&mut execution.report);
                return Ok(());
            }
        };
        for message in outcome.messages {
            tracing::warn!(level = ?message.level, message_bytes = message.text.len(), "ACP PostToolUse hook");
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

    pub(super) async fn pace_tool_call(&self, session: &SessionHandle) -> Result<(), SdkError> {
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

    pub(super) fn tool_name_from_call(call: &ProviderToolCall) -> &str {
        call.function
            .as_ref()
            .map(|function| function.name.as_str())
            .unwrap_or("unknown")
    }

    pub(super) async fn capture_apply_patch_snapshot(
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

    pub(super) async fn absolute_workspace_root(&self, session: &SessionHandle) -> Result<Option<PathBuf>, SdkError> {
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

    pub(super) fn tool_call_result_from_report(call: &ProviderToolCall, report: ToolExecutionReport) -> ToolCallResult {
        ToolCallResult {
            tool_call_id: call.id.clone(),
            llm_response: report.llm_response,
            audit_status: report.audit_status,
            audit_arguments: Self::raw_tool_arguments(call),
        }
    }

    pub(super) fn raw_tool_arguments(call: &ProviderToolCall) -> Vec<u8> {
        call.function
            .as_ref()
            .map_or_else(Vec::new, |function| function.arguments.as_bytes().to_vec())
    }

    pub(super) fn update_fields_from_report(report: &ToolExecutionReport) -> acp::ToolCallUpdateFields {
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
}

fn should_capture_apply_patch_snapshot(tool_name: &str, permission_overridden: bool, cancelled: bool) -> bool {
    tool_name == tools::APPLY_PATCH && !permission_overridden && !cancelled
}

fn should_run_post_tool_hook(status: &acp::ToolCallStatus) -> bool {
    matches!(status, acp::ToolCallStatus::Completed)
}

fn append_post_tool_hook_failure_note(report: &mut ToolExecutionReport) {
    report.llm_response.push_str(
        "\n\nPostToolUse hook failed after this tool completed; the tool result and any effects are preserved.",
    );
}

pub(super) fn append_cancellation_after_completion_note(report: &mut ToolExecutionReport, cancelled: bool) {
    if cancelled && matches!(report.status, acp::ToolCallStatus::Completed) {
        report
            .llm_response
            .push_str("\n\nThe turn was cancelled after this tool completed; its effects were applied.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_tool_hook_only_runs_for_successful_completion() {
        assert!(should_run_post_tool_hook(&acp::ToolCallStatus::Completed));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::Failed));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::InProgress));
        assert!(!should_run_post_tool_hook(&acp::ToolCallStatus::Pending));
    }

    #[test]
    fn post_tool_hook_failure_note_preserves_completed_report_and_audit_status() {
        let mut report = ToolExecutionReport::success(Vec::new(), Vec::new(), serde_json::json!({ "applied": true }));
        let raw_output_before_hook = report.raw_output.clone();

        append_post_tool_hook_failure_note(&mut report);

        assert_eq!(report.status, acp::ToolCallStatus::Completed);
        assert_eq!(report.audit_status, vtcode_safety::audit_log::ToolAuditStatus::Success);
        assert_eq!(report.raw_output, raw_output_before_hook);
        assert!(report.llm_response.contains("the tool result and any effects are preserved"));
    }
}
