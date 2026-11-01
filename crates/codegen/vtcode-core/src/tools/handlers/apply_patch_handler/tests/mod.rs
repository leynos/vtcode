//! Tests for the Codex-style `apply_patch` handler surface.
//!
//! Shared fixtures live here. Behaviour-specific tests are split into sibling
//! modules — handler-surface preconditions and the direct runtime — so each
//! file stays within the repository's 400-line limit.

use super::*;
use crate::config::constants::tools;
use crate::exec_policy::RejectConfig;
use crate::tools::handlers::adapter::DefaultToolSession;
use crate::tools::handlers::sandboxing::{SandboxConfig, SandboxType};
use crate::tools::handlers::tool_handler::{Constrained, ShellEnvironmentPolicy, TurnContext};
use std::sync::Arc;
use tempfile::TempDir;

mod direct_runtime;
mod handler_runtime;

#[test]
fn test_parse_apply_patch_command_direct() {
    let cmd = vec!["apply_patch".to_string(), "*** Begin Patch\n*** End Patch".to_string()];
    let (is_patch, content) = parse_apply_patch_command(&cmd);
    assert!(is_patch);
    assert!(content.is_some());
}

#[test]
fn test_parse_apply_patch_command_not_patch() {
    let cmd = vec!["ls".to_string(), "-la".to_string()];
    let (is_patch, content) = parse_apply_patch_command(&cmd);
    assert!(!is_patch);
    assert!(content.is_none());
}

#[test]
fn test_create_freeform_tool() {
    let tool = create_apply_patch_freeform_tool();
    assert_eq!(tool.name(), "apply_patch");
}

#[test]
fn test_create_json_tool() {
    let tool = create_apply_patch_json_tool();
    assert_eq!(tool.name(), "apply_patch");
}

#[test]
fn test_apply_patch_json_args_support_patch_alias() {
    let parsed: ApplyPatchToolArgs =
        serde_json::from_str(r#"{"patch":"*** Begin Patch\n*** End Patch\n"}"#).expect("json args should parse");

    assert_eq!(parsed.input, None);
    assert_eq!(parsed.patch.as_deref(), Some("*** Begin Patch\n*** End Patch\n"));
}

#[test]
fn wants_no_sandbox_approval_reject_respects_sandbox_flag() {
    let runtime = ApplyPatchRuntime::new();
    assert!(runtime.wants_no_sandbox_approval(AskForApproval::OnRequest));
    assert!(!runtime.wants_no_sandbox_approval(AskForApproval::Reject(RejectConfig {
        sandbox_approval: true,
        rules: false,
        request_permissions: false,
        mcp_elicitations: false,
    })));
    assert!(runtime.wants_no_sandbox_approval(AskForApproval::Reject(RejectConfig {
        sandbox_approval: false,
        rules: false,
        request_permissions: false,
        mcp_elicitations: false,
    })));
}

/// Parse the structured envelope out of a model-facing precondition
/// rejection.
pub(super) fn precondition_envelope(error: &ToolCallError) -> Value {
    match error {
        ToolCallError::RespondToModel(message) => {
            serde_json::from_str(message).expect("precondition rejection should serialize to a JSON envelope")
        }
        other => panic!("expected a model-facing rejection, got {other:?}"),
    }
}

/// Builds a `ToolInvocation` whose session and turn both point at
/// `workspace`, so the handler exercises the same workspace-containment and
/// precondition path a real turn would use.
pub(super) fn test_invocation(workspace: PathBuf, arguments: String) -> ToolInvocation {
    let session = Arc::new(DefaultToolSession::with_workspace(workspace.clone(), workspace.clone()));
    let turn = Arc::new(TurnContext {
        cwd: workspace,
        turn_id: "apply-patch-handler-runtime-test".to_string(),
        sub_id: None,
        shell_environment_policy: ShellEnvironmentPolicy::default(),
        approval_policy: Constrained::default(),
        codex_linux_sandbox_exe: None,
        sandbox_policy: Constrained::default(),
    });
    ToolInvocation {
        session,
        turn,
        tracker: None,
        call_id: "call-apply-patch".to_string(),
        tool_name: tools::APPLY_PATCH.to_string(),
        payload: ToolPayload::Function { arguments },
    }
}

/// Build the direct `ToolCtx` a runtime test needs for `workspace`.
pub(super) fn direct_runtime_ctx(workspace: &TempDir) -> ToolCtx {
    let session =
        Arc::new(DefaultToolSession::with_workspace(workspace.path().to_path_buf(), workspace.path().to_path_buf()));
    let turn = Arc::new(TurnContext {
        cwd: workspace.path().to_path_buf(),
        turn_id: "direct-apply-patch-test".to_string(),
        sub_id: None,
        shell_environment_policy: ShellEnvironmentPolicy::default(),
        approval_policy: Constrained::default(),
        codex_linux_sandbox_exe: None,
        sandbox_policy: Constrained::default(),
    });
    ToolCtx {
        session,
        turn,
        call_id: "call-1".to_string(),
        tool_name: "apply_patch".to_string(),
    }
}

/// Build the `SandboxAttempt` a direct runtime call needs for `workspace`.
///
/// The attempt borrows `policy`, so callers keep it alive for the call.
pub(super) fn sandbox_attempt<'a>(policy: &'a SandboxConfig, workspace: &'a TempDir) -> SandboxAttempt<'a> {
    SandboxAttempt {
        sandbox: SandboxType::None,
        policy,
        sandbox_cwd: workspace.path(),
        codex_linux_sandbox_exe: None,
    }
}
