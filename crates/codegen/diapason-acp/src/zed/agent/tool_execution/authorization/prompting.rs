//! Permission prompting for an authorized ACP tool call.
//!
//! These stages decide whether a call is announced as in progress and ask the
//! client, or a `PermissionRequest` hook, for a decision. They run after
//! admission has judged the call and before it executes.

use super::super::super::ZedAgent;
use super::super::cancelable_session_request;
use super::super::data::{ResolvedToolCall, ToolAuthorization, ToolExecutionContext, ToolPermissionPrompt};
use crate::acp;
use crate::acp::Error as SdkError;
use crate::permissions::PermissionToolContext;
use crate::reports::ToolExecutionReport;
use crate::tooling::ToolDescriptor;
use crate::zed::agent::log_suppressed_error;
use crate::zed::agent::warn_hook_message;
use crate::zed::constants::SESSION_WORKSPACE_RUNTIME_UNAVAILABLE_FAILURE;
use diapason_core::permissions::build_permission_request;
use serde_json::Value;

impl ZedAgent {
    /// Announces an authorized call to the client as in progress.
    ///
    /// A resolved call that will not execute settles through its own path, so
    /// it is not announced here: a refused or cancelled call must not look
    /// started, and the terminal update already names the call.
    pub(super) async fn send_tool_call_in_progress(
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

    /// Asks the client for permission, honouring the session's cancellation.
    ///
    /// A cancelled request reports the call as cancelled rather than leaving
    /// the prompt pending, because the client may never answer it.
    pub(super) async fn request_tool_permission_for_call(
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

    /// Runs the session's `PermissionRequest` hooks for one resolved call.
    ///
    /// A hook engine is optional and the workspace of the request is the
    /// session's own when it has a workspace runtime, so a hook evaluates the
    /// call against the workspace it would run in.
    pub(super) async fn run_permission_hook(
        &self,
        context: &ToolExecutionContext<'_>,
        resolved: &ResolvedToolCall<'_>,
        args: &Value,
    ) -> Result<Option<diapason_core::hooks::PermissionRequestHookDecision>, SdkError> {
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
