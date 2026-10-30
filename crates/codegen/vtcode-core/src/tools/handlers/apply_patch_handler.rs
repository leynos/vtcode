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
use serde_json::{Value, json};

use super::events::{ToolEmitter, ToolEventCtx};
use super::sandboxing::{
    Approvable, ApprovalCtx, AskForApproval, BoxFuture, ExecApprovalRequirement, ExecToolCallOutput, ReviewDecision,
    SandboxAttempt, Sandboxable, SandboxablePreference, ToolCtx, ToolError, ToolRuntime,
};
use super::tool_handler::{
    ApprovalPolicy, FileChange, FreeformTool, FreeformToolFormat, ResponsesApiTool, ToolCallError, ToolHandler,
    ToolInvocation, ToolKind, ToolOutput, ToolPayload, ToolSpec,
};
use super::tool_orchestrator::ToolOrchestrator;
use crate::config::constants::tools;
use crate::tools::editing::{Patch, PatchOperation};
use crate::tools::registry::patch_preconditions::{invalid_patch_precondition, patch_precondition_scope_error};

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
    /// The `expected_content_hash` precondition is enforced by the handler
    /// before the orchestrator reaches this runtime, because a rejection has to
    /// reach the model as a structured envelope rather than as a flat
    /// `ToolError` string.
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
        validate_request_content_hash(&req, &patch, session.workspace_root())
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

/// A rejected `expected_content_hash` precondition on the handler surface.
///
/// Structured failures carry the same envelope the registry pipeline returns,
/// so the model sees one payload shape whichever apply-patch surface rejected
/// its patch. Plain rejections cover containment and read failures, which have
/// no version metadata to report.
#[derive(Debug)]
enum PatchPreconditionFailure {
    /// A plain rejection for a path-containment or read failure.
    Rejected(String),
    /// The registry's structured, model-facing rejection envelope.
    Structured(Value),
}

impl PatchPreconditionFailure {
    /// Convert into the handler error the model is shown.
    fn into_tool_call_error(self) -> ToolCallError {
        match self {
            Self::Rejected(message) => ToolCallError::Rejected(message),
            // The adapter treats `RespondToModel` as a model-facing failure,
            // so the serialized envelope is not reclassified as internal.
            Self::Structured(envelope) => ToolCallError::RespondToModel(envelope.to_string()),
        }
    }
}

impl std::fmt::Display for PatchPreconditionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) => f.write_str(message),
            Self::Structured(envelope) => write!(f, "{envelope}"),
        }
    }
}

/// Enforce `expected_content_hash` on the Codex-style handler surface.
///
/// The file is read once: the same bytes produce the current hash and the
/// rebase assessment, so a concurrent writer cannot retarget a workspace
/// symlink between the containment check and a second open. A mismatch returns
/// the registry's structured envelope carrying the expected and current hashes
/// plus bounded anchor failures, which tells the model to reread rather than to
/// regenerate blindly. A file whose bytes are not UTF-8 cannot be assessed for
/// rebase, so it reports `can_safely_rebase: false` with no anchor failures,
/// matching the registry's `text_content == None` case.
async fn validate_request_content_hash(
    req: &ApplyPatchRequest,
    patch: &Patch,
    workspace_root: &Path,
) -> Result<(), PatchPreconditionFailure> {
    let Some(expected_hash) = req.expected_content_hash.as_deref() else {
        return Ok(());
    };
    let source_paths = crate::tools::apply_patch::patch_precondition_source_paths(patch);
    let [source_path] = source_paths.as_slice() else {
        let scope = patch_precondition_scope_error(source_paths.len());
        return Err(PatchPreconditionFailure::Structured(invalid_patch_precondition(
            scope.message,
            json!({
                "reason": "invalid_precondition_scope",
                "source_file_count": scope.source_file_count,
                "next_action": "Split the patch by source file, reread each file, and retry with its content_hash."
            }),
        )));
    };
    let disk_path = req.cwd.join(source_path);
    vtcode_commons::paths::ensure_path_within_workspace_resolved(&disk_path, workspace_root)
        .await
        .map_err(|error| {
            PatchPreconditionFailure::Rejected(format!(
                "apply_patch rejected precondition path '{source_path}' outside session workspace: {error}"
            ))
        })?;
    let bytes = tokio::fs::read(&disk_path).await.map_err(|error| {
        PatchPreconditionFailure::Rejected(format!("Failed to read {source_path} for hash validation: {error}"))
    })?;
    let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(&bytes));
    if current_hash == expected_hash {
        return Ok(());
    }
    // Reuse the bytes already read for hash validation instead of reopening the
    // path. A concurrent local writer could retarget a workspace symlink between
    // the containment check and a second read, which would let the rebase
    // assessment read a file outside the workspace and quote it back to the
    // model through the anchor failures.
    let (can_safely_rebase, anchor_failures) = match std::str::from_utf8(&bytes) {
        Ok(current) => crate::tools::apply_patch::assess_patch_rebase_with_content(patch, &disk_path, current).await,
        Err(_) => (false, Vec::new()),
    };
    let details = json!({
        "reason": "content_hash_mismatch",
        "path": source_path,
        "expected_content_hash": expected_hash,
        "current_content_hash": current_hash,
        "anchor_failures": anchor_failures,
        "can_safely_rebase": can_safely_rebase,
        "next_action": format!("Reread {source_path} and regenerate the patch using the new content_hash."),
    });
    Err(PatchPreconditionFailure::Structured(invalid_patch_precondition(
        format!("File version changed before apply_patch: {source_path}"),
        details,
    )))
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

/// Create freeform apply_patch tool spec (for GPT-5 style models)
pub fn create_apply_patch_freeform_tool() -> ToolSpec {
    ToolSpec::Freeform(FreeformTool {
        name: tools::APPLY_PATCH.to_string(),
        description: APPLY_PATCH_DESCRIPTION.to_string(),
        format: FreeformToolFormat {
            lark_grammar: Some(APPLY_PATCH_LARK_GRAMMAR.to_string()),
            examples: vec![
                APPLY_PATCH_ADD_EXAMPLE.to_string(),
                APPLY_PATCH_UPDATE_EXAMPLE.to_string(),
            ],
        },
    })
}

/// Create JSON function apply_patch tool spec (for standard function calling)
pub fn create_apply_patch_json_tool() -> ToolSpec {
    use crate::tools::apply_patch::{
        APPLY_PATCH_ALIAS_DESCRIPTION, DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION, with_semantic_anchor_guidance,
    };
    ToolSpec::Function(ResponsesApiTool {
        name: tools::APPLY_PATCH.to_string(),
        description: format!("{APPLY_PATCH_DESCRIPTION}\n\n{APPLY_PATCH_GRAMMAR_HELP}"),
        strict: false,
        parameters: json!({
            "type": "object",
            "properties": {
                "input": {
                    "type": "string",
                    "description": with_semantic_anchor_guidance(DEFAULT_APPLY_PATCH_INPUT_DESCRIPTION)
                },
                "patch": {
                    "type": "string",
                    "description": with_semantic_anchor_guidance(APPLY_PATCH_ALIAS_DESCRIPTION)
                }
            },
            "required": ["input"],
            "additionalProperties": false
        }),
    })
}

/// Parse a shell command to check if it's an apply_patch invocation
pub(crate) fn parse_apply_patch_command(command: &[String]) -> (bool, Option<String>) {
    const APPLY_PATCH_COMMANDS: &[&str] = &["apply_patch", "applypatch"];

    match command {
        // Direct invocation: apply_patch <patch>
        [cmd, body] if APPLY_PATCH_COMMANDS.contains(&cmd.as_str()) => (true, Some(body.clone())),
        // Shell heredoc form is not directly supported here
        // The Codex implementation uses tree-sitter to parse these
        _ => (false, None),
    }
}

// Constants for tool descriptions
const APPLY_PATCH_DESCRIPTION: &str = r#"Use the `apply_patch` tool to edit files.
Your patch language is a stripped-down, file-oriented diff format designed to be easy to parse and safe to apply. Every patch path must be workspace-relative; never use absolute paths, `..`, or traversal-like forms.

You can think of it as a high-level envelope:

*** Begin Patch
[ one or more file sections ]
*** End Patch

Within that envelope, you get a sequence of file operations.
You MUST include a header to specify the action you are taking.
Each operation starts with one of three headers:

*** Add File: <path> - create a new file. Every following line is a + line (the initial contents).
*** Delete File: <path> - remove an existing file. Nothing follows.
*** Update File: <path> - patch an existing file in place (optionally with a rename)."#;

const APPLY_PATCH_GRAMMAR_HELP: &str = r#"May be immediately followed by *** Move to: <new path> if you want to rename the file.
Then one or more "hunks", each introduced by @@ (optionally followed by a hunk header).
Within a hunk each line starts with:

- ` ` (space) for context lines
- `-` for lines to remove
- `+` for lines to add

Important rules:
- You must include a header with your intended action (Add/Delete/Update)
- You must prefix new lines with `+` even when creating a new file
- File references must be workspace-relative; never use absolute paths, `..`, or traversal-like forms
- Prefer small hunks with stable semantic @@ anchors like function, class, method, or impl names"#;

const APPLY_PATCH_LARK_GRAMMAR: &str = r#"
patch := "*** Begin Patch" NEWLINE { operation } "*** End Patch"
operation := AddFile | DeleteFile | UpdateFile
AddFile := "*** Add File: " path NEWLINE { "+" text NEWLINE }
DeleteFile := "*** Delete File: " path NEWLINE
UpdateFile := "*** Update File: " path NEWLINE [ MoveTo ] { Hunk }
MoveTo := "*** Move to: " newPath NEWLINE
Hunk := "@@" [ header ] NEWLINE { HunkLine } [ "*** End of File" NEWLINE ]
HunkLine := (" " | "-" | "+") text NEWLINE
"#;

const APPLY_PATCH_ADD_EXAMPLE: &str = r#"*** Begin Patch
*** Add File: hello.txt
+Hello world
*** End Patch"#;

const APPLY_PATCH_UPDATE_EXAMPLE: &str = r#"*** Begin Patch
*** Update File: src/app.py
*** Move to: src/main.py
@@ def greet():
-print("Hi")
+print("Hello, world!")
*** End Patch"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_policy::RejectConfig;
    use crate::tools::handlers::adapter::DefaultToolSession;
    // Only the `#[cfg(unix)]` symlink-escape test builds a `SandboxAttempt`, so
    // importing these unconditionally fails Windows' `-D warnings` build.
    #[cfg(unix)]
    use crate::tools::handlers::sandboxing::{SandboxConfig, SandboxType};
    use crate::tools::handlers::tool_handler::{Constrained, ShellEnvironmentPolicy, TurnContext};
    use std::sync::Arc;
    use tempfile::TempDir;

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

    /// Check a stale expected hash is rejected before the handler writes.
    #[tokio::test]
    async fn json_handler_enforces_expected_content_hash_before_apply() {
        let workspace = TempDir::new().expect("temporary workspace");
        std::fs::write(workspace.path().join("versioned.txt"), "current\n").expect("write fixture");
        let patch =
            Patch::parse("*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n")
                .expect("valid patch");
        let request = ApplyPatchRequest {
            patch: String::new(),
            expected_content_hash: Some(format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"))),
            cwd: workspace.path().to_path_buf(),
            timeout_ms: None,
            user_explicitly_approved: true,
        };

        let error = validate_request_content_hash(&request, &patch, workspace.path())
            .await
            .expect_err("stale handler precondition");

        let message = error.to_string();
        assert!(message.contains("content_hash_mismatch"));
        assert!(message.contains("current_content_hash"));
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("versioned.txt")).expect("read unchanged fixture"),
            "current\n"
        );
    }

    /// The JSON `expected_content_hash` precondition must be enforced by the
    /// handler *runtime*, not only by the precondition helper. This drives the
    /// real `ApplyPatchHandler::handle` entry point so the payload decoding,
    /// orchestrator, and runtime wiring are all covered.
    #[tokio::test]
    async fn handler_runtime_rejects_stale_expected_content_hash() {
        let workspace = TempDir::new().expect("temporary workspace");
        let fixture = workspace.path().join("versioned.txt");
        std::fs::write(&fixture, "current\n").expect("write fixture");
        let stale_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"stale\n"));
        let arguments = json!({
            "input": "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n",
            "expected_content_hash": stale_hash,
        })
        .to_string();

        let error = ApplyPatchHandler
            .handle(test_invocation(workspace.path().to_path_buf(), arguments))
            .await
            .expect_err("the handler must reject a stale expected_content_hash");

        let message = error.to_string();
        assert!(message.contains("content_hash_mismatch"), "unexpected error: {message}");
        assert!(message.contains("current_content_hash"), "unexpected error: {message}");
        assert_eq!(
            std::fs::read_to_string(&fixture).expect("read unchanged fixture"),
            "current\n",
            "a rejected precondition must not mutate the file"
        );
    }

    /// A matching `expected_content_hash` must let the handler through to the
    /// mutation itself, proving the guard is a precondition and not a blanket
    /// rejection of versioned patches.
    #[tokio::test]
    async fn handler_runtime_applies_patch_with_matching_expected_content_hash() {
        let workspace = TempDir::new().expect("temporary workspace");
        let fixture = workspace.path().join("versioned.txt");
        std::fs::write(&fixture, "current\n").expect("write fixture");
        let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"));
        let arguments = json!({
            "input": "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+changed\n*** End Patch\n",
            "expected_content_hash": current_hash,
        })
        .to_string();

        ApplyPatchHandler
            .handle(test_invocation(workspace.path().to_path_buf(), arguments))
            .await
            .expect("a matching content hash must allow the patch to apply");

        assert_eq!(
            std::fs::read_to_string(&fixture).expect("read patched fixture"),
            "changed\n",
            "the handler should have applied the patch"
        );
    }

    /// An add-only patch has no pre-existing content to version, so the handler
    /// must reject the hash with the registry's scope envelope rather than a
    /// generic message.
    #[tokio::test]
    async fn handler_runtime_rejects_expected_hash_on_add_only_patch() {
        let workspace = TempDir::new().expect("temporary workspace");
        let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"anything\n"));
        let arguments = json!({
            "input": "*** Begin Patch\n*** Add File: fresh.txt\n+hello\n*** End Patch\n",
            "expected_content_hash": expected_hash,
        })
        .to_string();

        let error = ApplyPatchHandler
            .handle(test_invocation(workspace.path().to_path_buf(), arguments))
            .await
            .expect_err("an add-only patch must reject expected_content_hash");

        let envelope = precondition_envelope(&error);
        let body = &envelope["error"];
        assert_eq!(body["error_type"], "InvalidParameters", "unexpected error: {envelope}");
        assert_eq!(body["details"]["reason"], "invalid_precondition_scope", "unexpected error: {envelope}");
        assert_eq!(body["details"]["source_file_count"], 0, "unexpected error: {envelope}");
        assert!(
            body["message"]
                .as_str()
                .expect("message should be a string")
                .contains("add-only"),
            "unexpected error: {envelope}"
        );
        assert!(!workspace.path().join("fresh.txt").exists(), "a rejected precondition must not add files");
    }

    /// A patch touching several source files has more than one version to check,
    /// so the handler must report the multi-source scope distinct from add-only.
    #[tokio::test]
    async fn handler_runtime_rejects_expected_hash_on_multi_source_patch() {
        let workspace = TempDir::new().expect("temporary workspace");
        for name in ["left.txt", "right.txt"] {
            std::fs::write(workspace.path().join(name), format!("{name}\n")).expect("write fixture");
        }
        let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"left.txt\n"));
        let arguments = json!({
            "input": "*** Begin Patch\n*** Update File: left.txt\n@@\n-left.txt\n+changed left\n*** Update File: right.txt\n@@\n-right.txt\n+changed right\n*** End Patch\n",
            "expected_content_hash": expected_hash,
        })
        .to_string();

        let error = ApplyPatchHandler
            .handle(test_invocation(workspace.path().to_path_buf(), arguments))
            .await
            .expect_err("a multi-source patch must reject expected_content_hash");

        let envelope = precondition_envelope(&error);
        let body = &envelope["error"];
        assert_eq!(body["error_type"], "InvalidParameters", "unexpected error: {envelope}");
        assert_eq!(body["details"]["reason"], "invalid_precondition_scope", "unexpected error: {envelope}");
        assert_eq!(body["details"]["source_file_count"], 2, "unexpected error: {envelope}");
        assert!(
            body["message"]
                .as_str()
                .expect("message should be a string")
                .contains("exactly one pre-existing source file"),
            "unexpected error: {envelope}"
        );
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("left.txt")).expect("read unchanged fixture"),
            "left.txt\n",
            "a rejected precondition must not mutate the file"
        );
    }

    /// A non-UTF-8 file cannot be assessed for rebase, so the mismatch must
    /// report the registry's binary shape: no safe rebase and no anchor
    /// failures, rather than a UTF-8 decode error.
    #[tokio::test]
    async fn handler_runtime_reports_binary_mismatch_shape() {
        let workspace = TempDir::new().expect("temporary workspace");
        let fixture = workspace.path().join("binary.bin");
        let bytes = [0xff_u8, 0xfe, 0xfd];
        std::fs::write(&fixture, bytes).expect("write binary fixture");
        let expected_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"expected text\n"));
        let arguments = json!({
            "input": "*** Begin Patch\n*** Update File: binary.bin\n@@\n-old\n+new\n*** End Patch\n",
            "expected_content_hash": expected_hash,
        })
        .to_string();

        let error = ApplyPatchHandler
            .handle(test_invocation(workspace.path().to_path_buf(), arguments))
            .await
            .expect_err("a binary mismatch must be rejected");

        let envelope = precondition_envelope(&error);
        let details = &envelope["error"]["details"];
        assert_eq!(details["reason"], "content_hash_mismatch", "unexpected error: {envelope}");
        assert_eq!(details["can_safely_rebase"], false, "unexpected error: {envelope}");
        assert_eq!(details["anchor_failures"], json!([]), "unexpected error: {envelope}");
        assert_eq!(
            details["current_content_hash"],
            format!("sha256:{}", vtcode_commons::utils::calculate_sha256(&bytes)),
            "unexpected error: {envelope}"
        );
        assert_eq!(
            std::fs::read(&fixture).expect("read unchanged fixture"),
            bytes,
            "a rejected precondition must not mutate the file"
        );
    }

    /// Parse the structured envelope out of a model-facing precondition
    /// rejection.
    fn precondition_envelope(error: &ToolCallError) -> Value {
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
    fn test_invocation(workspace: PathBuf, arguments: String) -> ToolInvocation {
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

    /// Check a symlinked cwd is rejected before the runtime mutates anything.
    #[cfg(unix)]
    #[tokio::test]
    async fn direct_runtime_rejects_symlink_escaped_cwd_before_mutation() {
        use std::os::unix::fs::symlink;

        let workspace = TempDir::new().expect("workspace should be created");
        let outside = TempDir::new().expect("outside directory should be created");
        let escaped_cwd = workspace.path().join("escaped");
        symlink(outside.path(), &escaped_cwd).expect("cwd symlink should be created");

        let session = Arc::new(DefaultToolSession::with_workspace(
            workspace.path().to_path_buf(),
            workspace.path().to_path_buf(),
        ));
        let turn = Arc::new(TurnContext {
            cwd: escaped_cwd.clone(),
            turn_id: "direct-apply-patch-test".to_string(),
            sub_id: None,
            shell_environment_policy: ShellEnvironmentPolicy::default(),
            approval_policy: Constrained::default(),
            codex_linux_sandbox_exe: None,
            sandbox_policy: Constrained::default(),
        });
        let tool_ctx = ToolCtx {
            session,
            turn,
            call_id: "call-1".to_string(),
            tool_name: "apply_patch".to_string(),
        };
        let policy = SandboxConfig::default();
        let attempt = SandboxAttempt {
            sandbox: SandboxType::None,
            policy: &policy,
            sandbox_cwd: workspace.path(),
            codex_linux_sandbox_exe: None,
        };
        let request = ApplyPatchRequest {
            patch: "*** Begin Patch\n*** Add File: created.txt\n+must not exist\n*** End Patch\n".to_string(),
            expected_content_hash: None,
            cwd: escaped_cwd,
            timeout_ms: None,
            user_explicitly_approved: true,
        };

        let error = ApplyPatchRuntime::new()
            .run(&request, &attempt, &tool_ctx)
            .await
            .expect_err("direct apply_patch must reject an escaped cwd");
        assert!(error.to_string().contains("outside session workspace"));
        assert!(!outside.path().join("created.txt").exists());
    }
}
