//! Resolve ACP tool calls and apply lifecycle and permission authorization.
use super::super::ZedAgent;
use super::super::warn_hook_message;
use super::cancelable_session_request;
use super::data::{
    PermissionResolutionInput, ResolvedToolCall, ToolAuthorization, ToolAuthorizationInput, ToolCallInvocation,
    ToolExecutionContext, ToolPermissionPrompt,
};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::permissions::PermissionToolContext;
use crate::reports::ToolExecutionReport;
use crate::tooling::{AcpToolRegistry, ToolDescriptor};
use crate::zed::agent::log_suppressed_error;
use crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE;
use crate::zed::types::ToolCallResult;
use serde_json::Value;
use std::sync::Arc;
use vtcode_core::config::constants::tools;
use vtcode_core::hooks::PreToolHookDecision;
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;
use vtcode_core::permissions::build_permission_request;

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
        let scoped_registry;
        let local_tool_registry = workspace_runtime
            .as_ref()
            .map_or(&self.local_tool_registry, |runtime| &runtime.local_tool_registry);
        let local_tool_registry = if workspace_runtime.is_some() {
            scoped_registry = local_tool_registry.with_subagent_controller(
                self.session_subagent_controller(invocation.session)
                    .map_err(|_| SdkError::internal_error())?,
            );
            &scoped_registry
        } else {
            local_tool_registry
        };
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
        let permission_result = cancelable_session_request(&context.invocation.session.cancellation, async {
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
        })
        .await;
        permission_result.unwrap_or_else(|| Ok(Some(ToolExecutionReport::cancelled(resolved.tool_name))))
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
                log_suppressed_error(&error, "session_workspace_runtime_read", "session_lock_unavailable");
                SdkError::internal_error().data(SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE)
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
            warn_hook_message("PermissionRequest", &message);
        }
        Ok(outcome.decision)
    }
}
