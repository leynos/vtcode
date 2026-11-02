//! Path assessment for a recognized workspace-scoped `rm` deletion.
//!
//! Assessment answers only whether every operand resolves inside the
//! workspace. Confirmation, approvals, configured deny rules, and sandbox
//! enforcement remain the responsibility of the caller.

use std::path::{Path, PathBuf};

use vtcode_commons::paths::{canonicalize_async, canonicalize_workspace};

use crate::sandboxing::PROTECTED_WRITABLE_ROOT_DIR_NAMES;

use super::super::command_validation::{build_candidate_path, normalize_workspace_root};

/// Path-aware outcome for a supported deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeletionDecision {
    /// Every operand resolves inside the workspace.
    Allowed {
        /// Normalized workspace paths that would be deleted.
        targets: Vec<PathBuf>,
    },
    /// An operand was refused.
    Denied {
        /// The operand as supplied by the caller.
        target: String,
        /// User-readable reason for the refusal.
        reason: String,
    },
}

/// Assess every operand of a supported deletion against the workspace.
///
/// Targets are resolved relative to `working_dir` and must remain inside
/// `workspace_root`, including through symlinks. The workspace root itself,
/// canonical aliases of it, the home directory, and protected metadata
/// directories (`.git`, `.vtcode`, `.codex`, `.agents`) are refused.
/// Nonexistent in-workspace targets are allowed because deletion of a file
/// that is already gone is harmless.
pub async fn assess_deletion_targets(
    operands: &[String],
    working_dir: &Path,
    workspace_root: &Path,
) -> DeletionDecision {
    let normalized_root = match normalize_workspace_root(workspace_root) {
        Ok(root) => root,
        Err(error) => {
            return DeletionDecision::Denied {
                target: workspace_root.display().to_string(),
                reason: format!("cannot resolve the workspace root: {error}"),
            };
        }
    };
    let canonical_root = canonicalize_workspace(&normalized_root);
    let canonical_home = match dirs::home_dir() {
        Some(home) => canonicalize_async(home).await.ok(),
        None => None,
    };

    let mut targets = Vec::with_capacity(operands.len());

    for operand in operands {
        match assess_operand(operand, working_dir, &normalized_root, &canonical_root, canonical_home.as_deref()).await {
            Ok(candidate) => targets.push(candidate),
            Err(denied) => return denied,
        }
    }

    DeletionDecision::Allowed { targets }
}

/// Resolve one operand and apply every containment check to it.
///
/// Returns the normalized candidate path when the operand is safe to delete,
/// or the denial for the caller to propagate unchanged.
async fn assess_operand(
    operand: &str,
    working_dir: &Path,
    normalized_root: &Path,
    canonical_root: &Path,
    canonical_home: Option<&Path>,
) -> Result<PathBuf, DeletionDecision> {
    let candidate = build_candidate_path(normalized_root, working_dir, operand)
        .await
        .map_err(|error| {
            denied(operand, format!("path is outside the workspace or escapes it through a symlink: {error}"))
        })?;

    if candidate == normalized_root {
        return Err(denied(operand, "the workspace root cannot be deleted".to_string()));
    }

    let mut resolved_target = None;
    match canonicalize_async(&candidate).await {
        Ok(resolved) => {
            if let Some(reason) = canonical_target_denial(&resolved, canonical_root, canonical_home) {
                return Err(denied(operand, reason.to_string()));
            }
            resolved_target = Some(resolved);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // A missing target is harmless: `rm -f` treats it as success.
        }
        Err(error) => {
            return Err(denied(operand, format!("cannot verify the target path: {error}")));
        }
    }

    // Check the resolved path as well so a symlink inside the workspace
    // cannot route a deletion into protected metadata. The resolved path
    // is canonical, so it must be compared against the canonical root;
    // stripping a lexically normalized root would silently skip the check
    // when the workspace path itself contains a symlink.
    let protected = protected_component(&candidate, normalized_root).or_else(|| {
        resolved_target
            .as_deref()
            .and_then(|resolved| protected_component(resolved, canonical_root))
    });
    if let Some(protected) = protected {
        return Err(denied(
            operand,
            format!("deletion inside protected metadata directory `{protected}` is not permitted"),
        ));
    }

    Ok(candidate)
}

/// Classify a canonical target that must not be deleted, if any.
fn canonical_target_denial(
    resolved: &Path,
    canonical_root: &Path,
    canonical_home: Option<&Path>,
) -> Option<&'static str> {
    if resolved == canonical_root {
        return Some("the target resolves to the workspace root");
    }
    if canonical_home.is_some_and(|home| resolved == home) {
        return Some("the home directory cannot be deleted");
    }
    if !resolved.starts_with(canonical_root) {
        return Some("the target resolves outside the workspace");
    }
    None
}

fn denied(operand: &str, reason: String) -> DeletionDecision {
    DeletionDecision::Denied { target: operand.to_string(), reason }
}

fn protected_component(candidate: &Path, root: &Path) -> Option<&'static str> {
    let relative = candidate.strip_prefix(root).ok()?;
    relative.components().find_map(|component| {
        let name = component.as_os_str().to_str()?;
        PROTECTED_WRITABLE_ROOT_DIR_NAMES
            .iter()
            .copied()
            .find(|protected| protected.eq_ignore_ascii_case(name))
    })
}
