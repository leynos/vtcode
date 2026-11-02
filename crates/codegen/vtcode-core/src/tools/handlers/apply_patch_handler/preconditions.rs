//! `expected_content_hash` enforcement for the Codex-style handler surface.
//!
//! The registry pipeline reaches the same verdict through captured snapshots;
//! this module derives it from a single read of the source file so both
//! surfaces return one payload shape. Kept out of `apply_patch_handler` itself
//! to hold each module within the repository's 400-line limit.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use super::super::sandboxing::ToolError;
use super::super::tool_handler::ToolCallError;
use crate::metrics::{MetricsCollector, PatchPreconditionOutcome};
use crate::tools::editing::Patch;
use crate::tools::registry::patch_preconditions::{
    content_hash_mismatch_details, invalid_patch_precondition, patch_precondition_scope_error,
    precondition_scope_details,
};

/// A rejected `expected_content_hash` precondition on the handler surface.
///
/// Structured failures carry the same envelope the registry pipeline returns,
/// so the model sees one payload shape whichever apply-patch surface rejected
/// its patch. Plain rejections cover containment and read failures, which have
/// no version metadata to report.
#[derive(Debug)]
pub(super) enum PatchPreconditionFailure {
    /// A plain rejection for a path-containment or read failure.
    Rejected(String),
    /// The registry's structured, model-facing rejection envelope.
    Structured {
        /// Bounded outcome category recorded in metrics.
        outcome: PatchPreconditionOutcome,
        /// The model-facing envelope.
        envelope: Value,
    },
}

impl PatchPreconditionFailure {
    /// The bounded outcome category this failure counts as.
    pub(super) fn outcome(&self) -> PatchPreconditionOutcome {
        match self {
            Self::Rejected(_) => PatchPreconditionOutcome::ValidationFailed,
            Self::Structured { outcome, .. } => *outcome,
        }
    }

    /// Convert into the handler error the model is shown.
    pub(super) fn into_tool_call_error(self) -> ToolCallError {
        match self {
            Self::Rejected(message) => ToolCallError::Rejected(message),
            // The adapter treats `RespondToModel` as a model-facing failure,
            // so the serialized envelope is not reclassified as internal.
            Self::Structured { envelope, .. } => ToolCallError::RespondToModel(envelope.to_string()),
        }
    }

    /// Convert into the runtime error the orchestrator reports.
    ///
    /// A structured failure keeps its model-facing envelope; a plain rejection
    /// stays a rejection so an approval decision is not implied by a read or
    /// containment failure.
    pub(super) fn into_tool_error(self) -> ToolError {
        match self {
            Self::Rejected(message) => ToolError::Rejected(message),
            Self::Structured { envelope, .. } => ToolError::RespondToModel(envelope.to_string()),
        }
    }
}

impl std::fmt::Display for PatchPreconditionFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(message) => f.write_str(message),
            Self::Structured { envelope, .. } => write!(f, "{envelope}"),
        }
    }
}

/// Counts one handler evaluation, including every early return.
///
/// `ApplyPatchHandler` runs outside the registry pipeline, so this guard is the
/// only place its evaluation duration is recorded. A guard rather than a call
/// at each exit keeps a rejected, failed, or successful request all counted
/// exactly once. The collector is optional so a session without telemetry
/// pays no cost.
pub(super) struct PatchHandlerEvaluationTimer {
    collector: Option<Arc<MetricsCollector>>,
    started: Instant,
}

impl PatchHandlerEvaluationTimer {
    /// Start timing one handler evaluation against `collector`.
    pub(super) fn new(collector: Option<Arc<MetricsCollector>>) -> Self {
        Self { collector, started: Instant::now() }
    }
}

impl Drop for PatchHandlerEvaluationTimer {
    fn drop(&mut self) {
        let Some(collector) = self.collector.as_ref() else {
            return;
        };
        let duration_ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        collector.record_patch_evaluation(duration_ms);
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
pub(super) async fn validate_request_content_hash(
    expected_content_hash: Option<&str>,
    patch: &Patch,
    cwd: &Path,
    workspace_root: &Path,
) -> Result<Option<PatchPreconditionOutcome>, PatchPreconditionFailure> {
    let Some(expected_hash) = expected_content_hash else {
        return Ok(None);
    };
    let source_paths = crate::tools::apply_patch::patch_precondition_source_paths(patch);
    let [source_path] = source_paths.as_slice() else {
        let scope = patch_precondition_scope_error(source_paths.len());
        return Err(PatchPreconditionFailure::Structured {
            outcome: PatchPreconditionOutcome::InvalidScope,
            envelope: invalid_patch_precondition(scope.message, precondition_scope_details(&scope)),
        });
    };
    let disk_path = cwd.join(source_path);
    vtcode_commons::paths::ensure_path_within_workspace_resolved(&disk_path, workspace_root)
        .await
        .map_err(|error| {
            PatchPreconditionFailure::Rejected(format!(
                "apply_patch rejected precondition path '{source_path}' outside session workspace: {error}"
            ))
        })?;
    let bytes = match tokio::fs::read(&disk_path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A removed file has no current version. Report it with a null
            // hash rather than the malformed empty `sha256:` value a vanished
            // read would otherwise produce.
            return Err(PatchPreconditionFailure::Structured {
                outcome: PatchPreconditionOutcome::MissingFile,
                envelope: invalid_patch_precondition(
                    format!("Source file no longer exists: {source_path}"),
                    content_hash_mismatch_details(source_path, expected_hash, None, false, json!([])),
                ),
            });
        }
        Err(error) => {
            return Err(PatchPreconditionFailure::Rejected(format!(
                "Failed to read {source_path} for hash validation: {error}"
            )));
        }
    };
    let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(&bytes));
    if current_hash == expected_hash {
        return Ok(Some(PatchPreconditionOutcome::Matched));
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
    let details = content_hash_mismatch_details(
        source_path,
        expected_hash,
        Some(&current_hash),
        can_safely_rebase,
        json!(anchor_failures),
    );
    Err(PatchPreconditionFailure::Structured {
        outcome: PatchPreconditionOutcome::ContentHashMismatch,
        envelope: invalid_patch_precondition(
            format!("File version changed before apply_patch: {source_path}"),
            details,
        ),
    })
}
