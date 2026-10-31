//! Data passed between ACP tool-execution stages.
use crate::acp;
use crate::permissions::{AcpPermissionPrompter, PermissionToolContext};
use crate::reports::ToolExecutionReport;
use crate::tooling::{AcpToolRegistry, SupportedTool, ToolDescriptor};
use crate::zed::connection::ConnectionHandle;
use crate::zed::types::SessionHandle;
use serde_json::Value;
use vtcode_core::hooks::{PermissionDecisionBehaviour, PreToolHookDecision};
use vtcode_core::llm::provider::ToolCall as ProviderToolCall;
use vtcode_core::tools::registry::ToolRegistry as CoreToolRegistry;

#[derive(Clone, Copy)]
pub(super) struct ToolCallInvocation<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) session_id: &'a acp::SessionId,
    pub(super) turn_id: &'a str,
    pub(super) call: &'a ProviderToolCall,
    pub(super) client: &'a ConnectionHandle,
}

pub(in crate::zed::agent) struct ToolAuditInput<'a> {
    pub(in crate::zed::agent) session: &'a SessionHandle,
    pub(in crate::zed::agent) session_id: &'a acp::SessionId,
    pub(in crate::zed::agent) turn_id: &'a str,
    pub(in crate::zed::agent) call: &'a ProviderToolCall,
    pub(in crate::zed::agent) effective_arguments: &'a [u8],
    pub(in crate::zed::agent) result: Result<(&'a str, vtcode_safety::audit_log::ToolAuditStatus), &'a str>,
    pub(in crate::zed::agent) duration_ms: u64,
}

pub(super) struct ToolExecutionContext<'a> {
    pub(super) invocation: ToolCallInvocation<'a>,
    pub(super) acp_tool_registry: &'a AcpToolRegistry,
    pub(super) permission_prompter: &'a dyn AcpPermissionPrompter,
    pub(super) local_tool_registry: &'a CoreToolRegistry,
}

pub(super) struct ToolAuthorizationInput<'a> {
    pub(super) pre_tool_decision: PreToolHookDecision,
    pub(super) audit_arguments: &'a mut Vec<u8>,
}

pub(super) struct PermissionResolutionInput<'a> {
    pub(super) pre_tool_decision: PreToolHookDecision,
    pub(super) hook_decision: Option<vtcode_core::hooks::PermissionRequestHookDecision>,
    pub(super) was_cancelled_before_permission: bool,
    pub(super) authorization: &'a mut ToolAuthorization,
}

pub(super) struct ToolPermissionPrompt<'a> {
    pub(super) descriptor: ToolDescriptor,
    pub(super) args: &'a Value,
    pub(super) force_prompt: bool,
}

pub(super) struct ResolvedToolCall<'a> {
    pub(super) tool_name: &'a str,
    pub(super) raw_arguments: &'a str,
    pub(super) descriptor: Option<ToolDescriptor>,
    pub(super) parsed_args: Option<Value>,
    pub(super) args_parse_error: Option<String>,
    pub(super) call_id: acp::ToolCallId,
    pub(super) kind: acp::ToolKind,
    pub(super) initial_call: acp::ToolCall,
}

pub(super) struct ToolAuthorization {
    pub(super) effective_args: Option<Value>,
    pub(super) audit_arguments: Vec<u8>,
    pub(super) permission_override: Option<ToolExecutionReport>,
    pub(super) cancel_after_permission: bool,
}

impl ToolAuthorization {
    pub(super) fn new(resolved: &ResolvedToolCall<'_>) -> Self {
        let effective_args = resolved.parsed_args.clone();
        let audit_arguments = effective_tool_arguments(resolved.raw_arguments, effective_args.as_ref());
        Self {
            effective_args,
            audit_arguments,
            permission_override: None,
            cancel_after_permission: false,
        }
    }

    pub(super) fn apply_permission_hook_decision(
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

pub(super) struct AuthorizedToolExecution {
    pub(super) report: ToolExecutionReport,
    pub(super) effective_args: Option<Value>,
    pub(super) audit_arguments: Vec<u8>,
}

pub(super) fn effective_tool_arguments(raw_arguments: &str, effective_args: Option<&Value>) -> Vec<u8> {
    effective_args
        .and_then(|arguments| serde_json::to_vec(arguments).ok())
        .unwrap_or_else(|| raw_arguments.as_bytes().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
}
