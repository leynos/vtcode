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
        let candidate = match build_candidate_path(&normalized_root, working_dir, operand).await {
            Ok(candidate) => candidate,
            Err(error) => {
                return DeletionDecision::Denied {
                    target: operand.clone(),
                    reason: format!("path is outside the workspace or escapes it through a symlink: {error}"),
                };
            }
        };

        if candidate == normalized_root {
            return DeletionDecision::Denied {
                target: operand.clone(),
                reason: "the workspace root cannot be deleted".to_string(),
            };
        }

        let mut resolved_target = None;
        match canonicalize_async(&candidate).await {
            Ok(resolved) => {
                if resolved == canonical_root {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the target resolves to the workspace root".to_string(),
                    };
                }
                if canonical_home.as_ref().is_some_and(|home| resolved == *home) {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the home directory cannot be deleted".to_string(),
                    };
                }
                if !resolved.starts_with(&canonical_root) {
                    return DeletionDecision::Denied {
                        target: operand.clone(),
                        reason: "the target resolves outside the workspace".to_string(),
                    };
                }
                resolved_target = Some(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A missing target is harmless: `rm -f` treats it as success.
            }
            Err(error) => {
                return DeletionDecision::Denied {
                    target: operand.clone(),
                    reason: format!("cannot verify the target path: {error}"),
                };
            }
        }

        // Check the resolved path as well so a symlink inside the workspace
        // cannot route a deletion into protected metadata. The resolved path
        // is canonical, so it must be compared against the canonical root;
        // stripping a lexically normalized root would silently skip the check
        // when the workspace path itself contains a symlink.
        let protected = protected_component(&candidate, &normalized_root).or_else(|| {
            resolved_target
                .as_deref()
                .and_then(|resolved| protected_component(resolved, &canonical_root))
        });
        if let Some(protected) = protected {
            return DeletionDecision::Denied {
                target: operand.clone(),
                reason: format!("deletion inside protected metadata directory `{protected}` is not permitted"),
            };
        }

        targets.push(candidate);
    }

    DeletionDecision::Allowed { targets }
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
