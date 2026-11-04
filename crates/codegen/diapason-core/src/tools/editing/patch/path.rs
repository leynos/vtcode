use std::path::{Component, Path};

use super::error::PatchError;

pub(crate) fn validate_patch_path(operation: &'static str, raw_path: &str) -> Result<(), PatchError> {
    if raw_path.is_empty() {
        return Err(PatchError::InvalidPath {
            operation,
            path: raw_path.to_string(),
            reason: "path is empty".to_string(),
        });
    }

    if raw_path.chars().any(|c| matches!(c, '\0' | '\r' | '\n' | '\t')) {
        return Err(PatchError::InvalidPath {
            operation,
            path: raw_path.to_string(),
            reason: "path contains control characters".to_string(),
        });
    }

    // Absolute paths are accepted at parse time: models commonly echo a
    // workspace-relative target as an absolute path (for example
    // `/home/user/project/README.md`), and rejecting it here forces an
    // unnecessary round-trip. Authoritative containment is enforced at apply
    // time by `applicator::ensure_target_within_workspace`, which resolves
    // symlinks and fails closed on anything outside the workspace. Lexical
    // `..` traversal is still rejected here because it can never produce a
    // safe target however it is later joined.
    let candidate = Path::new(raw_path);
    for component in candidate.components() {
        if component == Component::ParentDir {
            return Err(PatchError::InvalidPath {
                operation,
                path: raw_path.to_string(),
                reason: "path escapes workspace".to_string(),
            });
        }
    }

    if raw_path.contains("//") {
        return Err(PatchError::InvalidPath {
            operation,
            path: raw_path.to_string(),
            reason: "path contains consecutive separators".to_string(),
        });
    }

    Ok(())
}
