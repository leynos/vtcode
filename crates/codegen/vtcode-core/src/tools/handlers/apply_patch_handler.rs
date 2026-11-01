//! Apply patch handler (from Codex)
//!
//! Implements the apply_patch tool using the Codex-style handler pattern.
//! Supports both freeform and JSON function call formats.
//!
//! This surface enforces only the `expected_content_hash` version precondition.
//! The no-op ladder and mutation leases are registry session state, not handler
//! state; parity between this handler's guard and the registry's leases belongs
//! to the deferred architecture work (#168) and must not be assumed here.
//!
//! Based on [openai/codex] tool handler patterns (Apache-2.0).
//! Copyright 2025 OpenAI. See the repository `THIRD-PARTY-NOTICES` file for
//! full attribution.
//!
//! [openai/codex]: https://github.com/openai/codex

use hashbrown::HashMap;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::events::{ToolEmitter, ToolEventCtx};
use super::sandboxing::{
    Approvable, ApprovalCtx, AskForApproval, BoxFuture, ExecApprovalRequirement, ExecToolCallOutput, ReviewDecision,
    SandboxAttempt, Sandboxable, SandboxablePreference, ToolCtx, ToolError, ToolRuntime,
};
use super::tool_handler::{
    ApprovalPolicy, FileChange, ToolCallError, ToolHandler, ToolInvocation, ToolKind, ToolOutput, ToolPayload,
};
use super::tool_orchestrator::ToolOrchestrator;
use crate::tools::editing::{Patch, PatchOperation};

mod preconditions;
mod specs;
#[cfg(test)]
mod tests;

use preconditions::{PatchPreconditionFailure, validate_request_content_hash};
pub(crate) use specs::parse_apply_patch_command;
pub use specs::{create_apply_patch_freeform_tool, create_apply_patch_json_tool};

/// Apply patch handler
pub struct ApplyPatchHandler;

/// Arguments for apply_patch function call
#[derive(Debug, Deserialize, Serialize)]
pub struct ApplyPatchToolArgs {
    pub input: Option<String>,
    pub patch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_content_hash: Option<String>,
}

/// Request for apply_patch runtime
#[derive(Clone, Debug)]
pub struct ApplyPatchRequest {
    pub patch: String,
    pub expected_content_hash: Option<String>,
    pub cwd: PathBuf,
    pub timeout_ms: Option<u64>,
    pub user_explicitly_approved: bool,
}

/// Approval key for caching
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize)]
pub struct ApplyPatchApprovalKey {
    patch: String,
    cwd: PathBuf,
}

/// Apply patch runtime for orchestrated execution
#[derive(Default)]
pub struct ApplyPatchRuntime;

impl ApplyPatchRuntime {
    pub fn new() -> Self {
        Self
    }
}

impl Sandboxable for ApplyPatchRuntime {
    fn sandbox_preference(&self) -> SandboxablePreference {
        // Patches modify files, so we prefer auto sandbox
        SandboxablePreference::Auto
    }

    fn escalate_on_failure(&self) -> bool {
        // Allow escalation if sandbox fails
        true
    }
}

impl Approvable<ApplyPatchRequest> for ApplyPatchRuntime {
    type ApprovalKey = ApplyPatchApprovalKey;

    fn approval_key(&self, req: &ApplyPatchRequest) -> Self::ApprovalKey {
        ApplyPatchApprovalKey { patch: req.patch.clone(), cwd: req.cwd.clone() }
    }

    fn exec_approval_requirement(&self, _req: &ApplyPatchRequest) -> Option<ExecApprovalRequirement> {
        // Preserve existing behaviour from the legacy orchestrator path:
        // apply_patch is executed without additional approval prompts here.
        Some(ExecApprovalRequirement::Skip {
            bypass_sandbox: false,
            proposed_execpolicy_amendment: None,
        })
    }

    fn wants_no_sandbox_approval(&self, policy: AskForApproval) -> bool {
        match policy {
            AskForApproval::Never => false,
            AskForApproval::Reject(reject_config) => !reject_config.rejects_sandbox_approval(),
            AskForApproval::OnFailure => true,
            AskForApproval::OnRequest => true,
            AskForApproval::UnlessTrusted => true,
        }
    }

    fn start_approval_async<'a>(
        &'a mut self,
        _req: &'a ApplyPatchRequest,
        _ctx: ApprovalCtx<'a>,
    ) -> BoxFuture<'a, ReviewDecision> {
        Box::pin(async { ReviewDecision::Approved })
    }
}

#[async_trait]
impl ToolRuntime<ApplyPatchRequest, ExecToolCallOutput> for ApplyPatchRuntime {
    /// Validate the cwd, parse the patch, and apply it.
    ///
    /// The handler checks `expected_content_hash` before the orchestrator
    /// reaches this runtime, but this runtime re-validates it immediately
    /// before applying. A file read during the handler's check can change
    /// before the write. The recheck narrows that window but does not make the
    /// check and the write atomic; atomic check-and-write needs a mutation
    /// lease, which issue #168 tracks.
    async fn run(
        &mut self,
        req: &ApplyPatchRequest,
        _attempt: &SandboxAttempt<'_>,
        ctx: &ToolCtx,
    ) -> Result<ExecToolCallOutput, ToolError> {
        vtcode_commons::paths::ensure_path_within_workspace_resolved(&req.cwd, ctx.session.workspace_root())
            .await
            .map_err(|error| {
                ToolError::Rejected(format!(
                    "apply_patch rejected cwd '{}' outside session workspace '{}': {error}",
                    req.cwd.display(),
                    ctx.session.workspace_root().display()
                ))
            })?;

        // Parse and apply the patch
        let patch = Patch::parse(&req.patch).map_err(|e| ToolError::Rejected(format!("Failed to parse patch: {e}")))?;

        if patch.is_empty() {
            return Ok(ExecToolCallOutput {
                stdout: "Patch is empty, no changes applied".to_string(),
                stderr: String::new(),
                exit_code: 0,
            });
        }

        // Re-validate the version precondition immediately before mutating. The
        // handler checked it already, but a concurrent writer can change the
        // file between that check and this apply. The recheck narrows that
        // window; it does not make the check and the apply atomic (#168). The
        // verdict keeps the same shape the preflight would have returned, so a
        // structured failure stays a model-facing envelope rather than becoming
        // an approval rejection.
        validate_request_content_hash(
            req.expected_content_hash.as_deref(),
            &patch,
            &req.cwd,
            ctx.session.workspace_root(),
        )
        .await
        .map_err(PatchPreconditionFailure::into_tool_error)?;

        // Apply the patch
        match patch.apply(&req.cwd).await {
            Ok(results) => {
                let output = results.join("\n");
                Ok(ExecToolCallOutput {
                    stdout: output,
                    stderr: String::new(),
                    exit_code: 0,
                })
            }
            Err(e) => Ok(ExecToolCallOutput {
                stdout: String::new(),
                stderr: format!("Patch application failed: {e}"),
                exit_code: 1,
            }),
        }
    }
}

#[async_trait]
impl ToolHandler for ApplyPatchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    fn matches_kind(&self, payload: &ToolPayload) -> bool {
        matches!(payload, ToolPayload::Function { .. } | ToolPayload::Custom { .. })
    }

    async fn is_mutating(&self, _invocation: &ToolInvocation) -> bool {
        true // apply_patch always mutates
    }

    /// Decode the patch payload and run it through the orchestrator.
    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, ToolCallError> {
        let ToolInvocation {
            session,
            turn,
            tracker,
            call_id,
            tool_name,
            payload,
        } = invocation;

        // Extract patch input from payload
        let (patch_input, expected_content_hash) = match payload {
            ToolPayload::Function { arguments } => {
                let args: Value = serde_json::from_str(&arguments)
                    .map_err(|e| ToolCallError::respond(format!("Failed to parse function arguments: {e}")))?;
                let expected_content_hash = crate::tools::apply_patch::expected_content_hash_from_args(&args)
                    .map_err(|e| ToolCallError::respond(format!("Invalid expected_content_hash: {e}")))?
                    .map(ToOwned::to_owned);
                let patch_input = crate::tools::apply_patch::decode_apply_patch_input(&args)
                    .map_err(|e| ToolCallError::respond(format!("Failed to decode patch input: {e}")))?
                    .map(|input| input.text)
                    .ok_or_else(|| {
                        ToolCallError::respond(format!(
                            "Missing patch input {}",
                            crate::tools::error_helpers::PATCH_PARAMETER_HINT
                        ))
                    })?;
                (patch_input, expected_content_hash)
            }
            ToolPayload::Custom { input } => (input, None),
            _ => {
                return Err(ToolCallError::respond("apply_patch handler received unsupported payload"));
            }
        };

        // Parse the patch to get file changes
        let patch =
            Patch::parse(&patch_input).map_err(|e| ToolCallError::respond(format!("Failed to parse patch: {e}")))?;

        // Create request
        let req = ApplyPatchRequest {
            patch: patch_input.clone(),
            expected_content_hash,
            cwd: turn.cwd.clone(),
            timeout_ms: None,
            user_explicitly_approved: true,
        };

        // Enforce the version precondition before any mutation event is emitted
        // so a rejection reaches the model as the registry's structured
        // envelope rather than a flat `ToolError::Rejected` string.
        validate_request_content_hash(req.expected_content_hash.as_deref(), &patch, &req.cwd, session.workspace_root())
            .await
            .map_err(PatchPreconditionFailure::into_tool_call_error)?;

        // Convert patch operations to file changes for tracking
        let changes = convert_patch_to_changes(&patch, &turn.cwd);

        // Create emitter for event tracking
        let emitter = ToolEmitter::apply_patch(changes.clone(), true);
        let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, tracker.as_ref());
        emitter.begin(event_ctx).await;

        // Execute using orchestrator
        let mut orchestrator = ToolOrchestrator::new();
        let mut runtime = ApplyPatchRuntime::new();
        let tool_ctx = ToolCtx {
            session: session.clone(),
            turn: turn.clone(),
            call_id: call_id.clone(),
            tool_name: tool_name.clone(),
        };

        let result = orchestrator
            .run(&mut runtime, &req, &tool_ctx, turn.as_ref(), map_approval_policy(turn.approval_policy.value()))
            .await;

        // Emit completion event and format output
        let event_ctx = ToolEventCtx::new(session.as_ref(), turn.as_ref(), &call_id, tracker.as_ref());
        let content = emitter.finish(event_ctx, result).await?;

        Ok(ToolOutput::Function { content, content_items: None, success: Some(true) })
    }
}

/// Convert patch operations to file changes for tracking
fn convert_patch_to_changes(patch: &Patch, cwd: &Path) -> HashMap<PathBuf, FileChange> {
    let mut changes = HashMap::new();

    for op in patch.operations() {
        match op {
            PatchOperation::AddFile { path, content } => {
                let full_path = cwd.join(path);
                changes.insert(full_path, FileChange::Add { content: content.clone() });
            }
            PatchOperation::DeleteFile { path } => {
                let full_path = cwd.join(path);
                changes.insert(full_path, FileChange::Delete);
            }
            PatchOperation::UpdateFile { path, new_path, chunks: _ } => {
                let full_path = cwd.join(path);
                if let Some(new_path) = new_path {
                    changes.insert(full_path, FileChange::Rename { new_path: cwd.join(new_path), content: None });
                } else {
                    // For updates, we track as update with empty placeholders
                    // The actual content will be computed during application
                    changes.insert(
                        full_path,
                        FileChange::Update {
                            old_content: String::new(),
                            new_content: String::new(),
                        },
                    );
                }
            }
        }
    }

    changes
}

fn map_approval_policy(policy: ApprovalPolicy) -> AskForApproval {
    match policy {
        ApprovalPolicy::Never => AskForApproval::Never,
        ApprovalPolicy::OnMutation => AskForApproval::OnRequest,
        ApprovalPolicy::Always => AskForApproval::UnlessTrusted,
    }
}
