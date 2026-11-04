//! Centralized exclusion constants and helpers for file traversal.
//!
//! All directory walkers, grep invocations, and file-operation tools should
//! reference these constants instead of maintaining their own skip lists.

use std::path::Path;

/// Directories skipped by default during workspace traversal.
///
/// This covers build artifacts, dependency stores, VCS metadata, and IDE
/// configuration directories that are almost never relevant to code search
/// or analysis.
pub const DEFAULT_EXCLUDED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    ".next",
    "vendor",
    ".cursor",
    ".diapason",
    ".vtcode",
    ".vscode",
    ".idea",
];

/// Sensitive files that must never be exposed in listings, search results,
/// or the TUI file palette.  These contain secrets, credentials, or
/// environment-specific configuration.
pub const SENSITIVE_FILES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.test",
    ".DS_Store",
    ".git-credentials",
    ".netrc",
    ".npmrc",
    ".pypirc",
    "credentials",
    "credentials.json",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "id_rsa",
];

/// Glob patterns passed to ripgrep (or other search back-ends) to exclude
/// noisy vendor/build directories from results.
pub const DEFAULT_IGNORE_GLOBS: &[&str] = &[
    "**/.git/**",
    "**/node_modules/**",
    "**/target/**",
    "**/.cursor/**",
    "**/dist/**",
    "**/.next/**",
    "**/vendor/**",
    "**/.diapason/**",
    "**/.vscode/**",
    "**/.idea/**",
];

/// Whether a directory is excluded, including the configured worktree data root.
pub fn is_excluded_directory_name(name: &str) -> bool {
    is_excluded_directory_name_with_workspace_dir(name, &crate::workspace_dir_name())
}

fn is_excluded_directory_name_with_workspace_dir(name: &str, workspace_dir: &Path) -> bool {
    DEFAULT_EXCLUDED_DIRS.contains(&name) || name.eq_ignore_ascii_case(".vtcode") || workspace_dir == Path::new(name)
}

/// Default ignore globs with the configured worktree data directory included.
pub fn default_ignore_globs() -> Vec<String> {
    let mut globs: Vec<String> = DEFAULT_IGNORE_GLOBS.iter().map(|pattern| (*pattern).to_owned()).collect();
    let custom = format!("**/{}/**", crate::workspace_dir_name().display());
    if !globs.contains(&custom) {
        globs.push(custom);
    }
    globs
}

/// Returns `true` if `name` matches any entry in [`SENSITIVE_FILES`] or
/// starts with `.env.` (catches all dotenv variants). Matching is
/// case-insensitive because macOS and Windows commonly use case-insensitive
/// filesystems.
pub fn is_sensitive_file(name: &str) -> bool {
    SENSITIVE_FILES.iter().any(|sensitive| name.eq_ignore_ascii_case(sensitive))
        || name.get(..5).is_some_and(|prefix| prefix.eq_ignore_ascii_case(".env."))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_excluded_directory_name_with_workspace_dir, is_sensitive_file};

    #[test]
    fn workspace_directory_exclusion_policy_preserves_public_dot_directories() {
        let workspace_dir = Path::new(".metadata");

        for legacy_name in [".vtcode", ".VtCoDe", ".VTCODE"] {
            assert!(
                is_excluded_directory_name_with_workspace_dir(legacy_name, workspace_dir),
                "legacy metadata directory {legacy_name} must be excluded"
            );
        }

        assert!(is_excluded_directory_name_with_workspace_dir(".metadata", workspace_dir));
        assert!(is_excluded_directory_name_with_workspace_dir(".diapason", workspace_dir));
        assert!(!is_excluded_directory_name_with_workspace_dir(".public", workspace_dir));
    }

    #[test]
    fn sensitive_file_matching_is_case_insensitive() {
        assert!(is_sensitive_file(".ENV"));
        assert!(is_sensitive_file(".Env.Local"));
        assert!(is_sensitive_file(".NPMRC"));
        assert!(!is_sensitive_file(".environment"));
    }

    #[test]
    fn ssh_private_key_basenames_are_sensitive() {
        assert!(is_sensitive_file("id_dsa"));
        assert!(is_sensitive_file("id_ecdsa"));
        assert!(is_sensitive_file("ID_ECDSA"));
        assert!(!is_sensitive_file("id_ecdsa.pub"));
    }
}
