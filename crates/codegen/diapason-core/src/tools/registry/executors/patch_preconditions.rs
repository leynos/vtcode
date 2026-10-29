//! Error builders for apply-patch precondition failures.
//!
//! These construct the structured `Value` payloads the model sees when a patch
//! is rejected before any write happens. They live here rather than in
//! `patch_pipeline` so that module stays within the 400-line limit.

use super::ToolRegistry;
use super::patch_pipeline::CapturedPatchPath;
use crate::tools::registry::{ToolErrorType, ToolExecutionError};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};

pub(super) async fn validate_patch_content_hash(
    registry: &ToolRegistry,
    patch: &crate::tools::editing::Patch,
    expected_hash: &str,
    captured_paths: &[CapturedPatchPath],
) -> Result<Option<Value>> {
    let source_paths = crate::tools::apply_patch::patch_precondition_source_paths(patch);
    let [source_path] = source_paths.as_slice() else {
        let message = if source_paths.is_empty() {
            "expected_content_hash is not valid for an add-only patch"
        } else {
            "expected_content_hash requires exactly one pre-existing source file; split the patch by source file"
        };
        return Ok(Some(invalid_patch_precondition(
            message,
            json!({
                "reason": "invalid_precondition_scope",
                "source_file_count": source_paths.len(),
                "next_action": "Split the patch by source file, reread each file, and retry with its content_hash."
            }),
        )));
    };
    let canonical_path = registry.file_ops_tool().normalize_user_path(source_path).await?;
    let snapshot = captured_paths
        .iter()
        .find(|captured| captured.path == canonical_path)
        .map(|captured| &captured.snapshot)
        .ok_or_else(|| anyhow!("Missing captured patch state for {}", canonical_path.display()))?;
    let current_hash = format!("sha256:{}", snapshot.sha256);
    if current_hash == expected_hash {
        return Ok(None);
    }

    let (can_safely_rebase, anchor_failures) = match snapshot.text_content.as_deref() {
        Some(current) => {
            crate::tools::apply_patch::assess_patch_rebase_with_content(patch, &canonical_path, current).await
        }
        None => (false, Vec::new()),
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
    Ok(Some(invalid_patch_precondition(
        format!("File version changed before apply_patch: {source_path}"),
        details,
    )))
}

pub(super) fn invalid_patch_precondition(message: impl Into<String>, details: Value) -> Value {
    ToolExecutionError::new(crate::config::constants::tools::APPLY_PATCH, ToolErrorType::InvalidParameters, message)
        .with_details(details)
        .to_json_value()
}

pub(super) fn repeated_no_op_error(signature: String, occurrence: u8) -> Value {
    ToolExecutionError::new(
        crate::config::constants::tools::APPLY_PATCH,
        ToolErrorType::PolicyViolation,
        "Repeated identical no-op patch blocked; reread the files or submit a materially different patch.",
    )
    .with_details(json!({
        "reason": "repeated_identical_no_op",
        "no_op": true,
        "occurrence": occurrence,
        "signature": signature,
        "retry_prohibited": true,
        "next_action": "Reread the affected files or regenerate a materially different patch before calling apply_patch again."
    }))
    .to_json_value()
}
