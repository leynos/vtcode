//! Resolve ACP tool calls and apply lifecycle and permission authorization.

mod admission;
mod prompting;

use self::admission::{AdmissionArguments, session_subagent_admission_error, session_subagent_controller_error};
use super::super::ZedAgent;
use super::super::warn_hook_message;
use super::data::{
    PermissionResolutionInput, ResolvedToolCall, ToolAuthorization, ToolAuthorizationInput, ToolCallInvocation,
    ToolExecutionContext, ToolPermissionPrompt,
};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::reports::ToolExecutionReport;
use crate::tooling::{AcpToolRegistry, ToolDescriptor};
use crate::zed::agent::log_suppressed_error;
use crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE;
use crate::zed::types::ToolCallResult;
use diapason_core::hooks::PreToolHookDecision;
use diapason_core::llm::provider::ToolCall as ProviderToolCall;
use serde_json::Value;
use std::sync::Arc;

impl ZedAgent {
    pub(super) async fn execute_tool_call_inner(
        &self,
        invocation: ToolCallInvocation<'_>,
        audit_arguments: &mut Vec<u8>,
    ) -> Result<ToolCallResult, SdkError> {
        let workspace_runtime = invocation.session.workspace_runtime().map_err(|error| {
            log_suppressed_error(&error, "session_workspace_runtime_read", "session_lock_unavailable");
            SdkError::internal_error().data(SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE)
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
        // Subagent work runs through the session's controller, never the
        // workspace's ownerless one; without one, core rejects the call.
        let session_controller = if workspace_runtime.is_some() {
            self.session_subagent_controller(invocation.session).map_err(|error| {
                log_suppressed_error(&error, "session_subagent_controller_read", "session_lock_unavailable");
                SdkError::internal_error()
            })?
        } else {
            None
        };
        let scoped_registry = local_tool_registry.with_subagent_controller(session_controller.clone());
        let Some(func_ref) = invocation.call.function.as_ref() else {
            return Ok(Self::tool_call_result_from_report(
                invocation.call,
                ToolExecutionReport::failure("unknown", "Malformed tool call: missing function payload"),
            ));
        };
        let tool_name = func_ref.name.as_str();
        let context = ToolExecutionContext {
            invocation,
            acp_tool_registry,
            permission_prompter,
            local_tool_registry: &scoped_registry,
        };
        // Resolve before admission so a rejection can announce the call it is
        // about to fail. `run_pre_tool_stage` publishes the initial tool call
        // on the executing path; without the same publication here the client
        // would receive a terminal update for a call id it never saw start.
        let resolved = Self::resolve_tool_call(invocation.call.id.as_str(), func_ref, context.acp_tool_registry);
        // Fail closed on a missing session controller before any hook or
        // prompt runs, but do not judge target ownership here: the raw target
        // may be obsolete, and an approved `PermissionRequest` hook may replace
        // it. Ownership is re-checked on the dispatched arguments after any
        // rewrite in `authorize_tool_call`.
        let admission = session_subagent_controller_error(tool_name, session_controller.as_ref());
        if let Some(rejection) = admission {
            // The call is settled here rather than executed, so it must report
            // the resolved call followed by the same terminal update a refused
            // call reports through the authorization path; otherwise the client
            // sees a call that never starts and never finishes even though the
            // model received the tool failure.
            self.send_update(invocation.session_id, acp::SessionUpdate::ToolCall(resolved.initial_call.clone()))
                .await?;
            let report = ToolExecutionReport::failure(resolved.tool_name, &rejection);
            let update = acp::ToolCallUpdate::new(resolved.call_id.clone(), Self::update_fields_from_report(&report));
            self.send_update(invocation.session_id, acp::SessionUpdate::ToolCallUpdate(update))
                .await?;
            return Ok(Self::tool_call_result_from_report(invocation.call, report));
        }
        let pre_tool_decision = self.run_pre_tool_stage(&context, &resolved).await?;

        if matches!(pre_tool_decision, PreToolHookDecision::Deny) {
            let report = ToolExecutionReport::blocked(resolved.tool_name, "Tool execution denied by PreToolUse hook");
            let update = acp::ToolCallUpdate::new(resolved.call_id.clone(), Self::update_fields_from_report(&report));
            self.send_update(invocation.session_id, acp::SessionUpdate::ToolCallUpdate(update))
                .await?;
            return Ok(Self::tool_call_result_from_report(invocation.call, report));
        }

        let authorization = self
            .authorize_tool_call(
                &context,
                &resolved,
                session_controller.as_ref(),
                ToolAuthorizationInput { pre_tool_decision, audit_arguments },
            )
            .await?;
        let execution = Box::pin(self.execute_authorized_tool(&context, &resolved, authorization)).await?;
        self.finish_tool_call(&context, &resolved, execution).await
    }

    fn resolve_tool_call<'a>(
        call_id: &str,
        function: &'a diapason_commons::llm::FunctionCall,
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
                warn_hook_message("PreToolUse", &message);
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
        session_controller: Option<&Arc<diapason_core::subagents::SubagentController>>,
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
        // A PermissionRequest hook may rewrite the dispatched arguments, so the
        // admission decision taken before the hook no longer describes the call
        // that would run. Re-admit the final arguments before the call is
        // marked approved or announced as in progress; a rewrite must not
        // smuggle in a target this session cannot own. A call already refused
        // or cancelled will not execute, so it needs no second decision.
        let re_admission_rejection =
            if authorization.permission_override.is_none() && !authorization.cancel_after_permission {
                let final_arguments = dispatched_arguments(resolved, &authorization);
                session_subagent_admission_error(resolved.tool_name, session_controller, final_arguments).await
            } else {
                None
            };
        if let Some(rejection) = re_admission_rejection {
            authorization.permission_override = Some(ToolExecutionReport::failure(resolved.tool_name, &rejection));
        }
        // A cancel can land while the ownership lookup above is awaited, and
        // the snapshot taken before it only sees a cancellation that had
        // already happened. Refresh it here, before the call is marked
        // approved or announced as in progress; otherwise a cancelled call
        // would still execute and only be noticed after completion.
        refresh_cancellation_after_re_admission(&mut authorization, &context.invocation.session.cancellation);
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
    ) -> Result<Option<diapason_core::hooks::PermissionRequestHookDecision>, SdkError> {
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
}

/// Borrows the arguments the call would actually dispatch.
///
/// Admission runs before resolution on the call's raw text and again after the
/// permission stages, where a hook may have replaced the arguments. Both checks
/// must judge the same input the executor will receive, so the second check
/// hands over the installed `effective_args` rather than re-reading the model's
/// original text.
///
/// The value is borrowed, not rendered: the executor is handed the parsed value
/// itself, so a re-serialized copy would only add work — and an allocation — to
/// every ordinary tool call. A call whose arguments never parsed has no parsed
/// value to hand over, and is judged on its raw text exactly as before, so the
/// executor's malformed-argument diagnostic still describes the text the model
/// sent.
fn dispatched_arguments<'a>(
    resolved: &'a ResolvedToolCall<'_>,
    authorization: &'a ToolAuthorization,
) -> AdmissionArguments<'a> {
    authorization
        .effective_args
        .as_ref()
        .map_or(AdmissionArguments::Raw(resolved.raw_arguments), AdmissionArguments::Parsed)
}

/// Re-reads the session's cancellation after the re-admission lookup.
///
/// The snapshot taken before the lookup misses a cancel that lands while it is
/// awaited. Refreshing afterwards, before the call is announced as in progress,
/// keeps an already-cancelled call from executing for its full duration and
/// being noticed only once it completes.
fn refresh_cancellation_after_re_admission(
    authorization: &mut ToolAuthorization,
    cancellation: &crate::zed::types::SessionCancellation,
) {
    authorization.cancel_after_permission |= cancellation.is_cancelled();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::zed::types::SessionCancellation;
    use std::sync::Arc;
    use tokio::sync::Notify;

    /// A cancel can land while the ownership lookup is awaited, after the
    /// permission-stage snapshot has already been taken. The refresh that runs
    /// once the lookup returns must observe it, so the call is not announced as
    /// in progress and then executed despite the cancellation.
    #[tokio::test]
    async fn a_cancel_during_the_ownership_lookup_is_observed_after_re_admission() {
        let cancellation = SessionCancellation::default();
        let mut authorization = ToolAuthorization {
            effective_args: None,
            audit_arguments: Vec::new(),
            permission_override: None,
            cancel_after_permission: false,
        };
        // Stand in for `session_subagent_admission_error`: the lookup is pending
        // while core searches the controller tree, exactly the window the
        // permission snapshot cannot see.
        let lookup_started = Arc::new(Notify::new());
        let started = Arc::clone(&lookup_started);
        let lookup_cancellation = cancellation.clone();
        let lookup = async move {
            started.notify_one();
            lookup_cancellation.cancelled().await;
            Option::<String>::None
        };
        let cancel = async {
            lookup_started.notified().await;
            cancellation.cancel();
        };

        let (rejection, ()) = tokio::join!(
            async {
                let rejection = lookup.await;
                refresh_cancellation_after_re_admission(&mut authorization, &cancellation);
                rejection
            },
            cancel
        );

        assert!(rejection.is_none(), "the lookup itself never observes the cancellation");
        assert!(
            authorization.cancel_after_permission,
            "a cancellation that landed during re-admission must be refreshed before the in-progress notification"
        );
    }
}
