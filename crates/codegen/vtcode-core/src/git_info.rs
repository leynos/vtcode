//! Git repository information collection
//!
//! This module provides utilities for collecting git metadata from the workspace,
//! similar to OpenAI Codex PR #10145. It collects remote URLs, HEAD commit hash,
//! and repository root path for inclusion in LLM request headers.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

/// Git repository information for a workspace
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GitInfo {
    /// Remote URLs keyed by remote name (e.g., "origin")
    pub remotes: BTreeMap<String, String>,
    /// HEAD commit hash (short form)
    pub head_commit: Option<String>,
    /// Repository root path
    pub repo_root: Option<String>,
}

/// Get git remote URLs for fetch remotes in the repository at the given path.
/// Returns a BTreeMap mapping remote names to their fetch URLs.
///
/// # Arguments
/// * `cwd` - The working directory to run git commands in
///
/// # Returns
/// A BTreeMap where keys are remote names (e.g., "origin") and values are fetch URLs.
/// Returns an empty map if not in a git repository or if no remotes are configured.
pub fn get_git_remote_urls(cwd: &Path) -> Result<BTreeMap<String, String>> {
    let output = Command::new("git")
        .args(["remote", "-v"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run git remote -v in {}", cwd.display()))?;

    if !output.status.success() {
        // Not a git repository or git not available
        return Ok(BTreeMap::new());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut remotes = BTreeMap::new();

    // Parse output like:
    // origin  https://github.com/user/repo.git (fetch)
    // origin  https://github.com/user/repo.git (push)
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            let name = parts[0].to_string();
            let url = parts[1].to_string();
            let purpose = parts[2].trim_matches(|c| c == '(' || c == ')');

            // Only collect fetch remotes to avoid duplicates
            if purpose == "fetch" {
                remotes.insert(name, url);
            }
        }
    }

    Ok(remotes)
}

/// Get the HEAD commit hash (short form) for the repository at the given path.
///
/// # Arguments
/// * `cwd` - The working directory to run git commands in
///
/// # Returns
/// The short commit hash (7 characters) of HEAD, or None if not in a git repository.
pub fn get_head_commit_hash(cwd: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run git rev-parse in {}", cwd.display()))?;

    if !output.status.success() {
        return Ok(None);
    }

    let hash = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if hash.is_empty() { Ok(None) } else { Ok(Some(hash)) }
}

/// Get the repository root path for the given working directory.
///
/// # Arguments
/// * `cwd` - The working directory to run git commands in
///
/// # Returns
/// The absolute path to the repository root, or None if not in a git repository.
pub fn get_git_repo_root(cwd: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run git rev-parse --show-toplevel in {}", cwd.display()))?;

    if !output.status.success() {
        return Ok(None);
    }

    let root = String::from_utf8_lossy(&output.stdout).trim().to_string();

    if root.is_empty() { Ok(None) } else { Ok(Some(root)) }
}

/// Collect all git information for a workspace.
///
/// # Arguments
/// * `cwd` - The working directory to collect git info from
///
/// # Returns
/// A GitInfo struct containing remote URLs, HEAD commit hash, and repo root.
/// Returns default GitInfo if not in a git repository.
pub fn collect_git_info(cwd: &Path) -> Result<GitInfo> {
    let remotes = get_git_remote_urls(cwd)?;
    let head_commit = get_head_commit_hash(cwd)?;
    let repo_root = get_git_repo_root(cwd)?;

    Ok(GitInfo { remotes, head_commit, repo_root })
}

/// Check if the given path is inside a git repository.
///
/// # Arguments
/// * `cwd` - The working directory to check
///
/// # Returns
/// true if inside a git repository, false otherwise.
#[must_use]
pub fn is_git_repo(cwd: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Async variant of [`get_git_remote_urls`]. Runs the git subprocess on the
/// blocking thread pool.
pub async fn get_git_remote_urls_async(cwd: std::path::PathBuf) -> Result<BTreeMap<String, String>> {
    tokio::task::spawn_blocking(move || get_git_remote_urls(&cwd))
        .await
        .context("Git remote URLs task panicked")?
}

/// Async variant of [`get_head_commit_hash`]. Runs the git subprocess on the
/// blocking thread pool.
pub async fn get_head_commit_hash_async(cwd: std::path::PathBuf) -> Result<Option<String>> {
    tokio::task::spawn_blocking(move || get_head_commit_hash(&cwd))
        .await
        .context("Git HEAD hash task panicked")?
}

#[cfg(test)]
#[path = "git_info_test_support.rs"]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    //! Verifies Git information collection against hermetic local repositories.

    use super::test_support::{
        CLEAN_TEST_CHILD_ENV, FIXTURE_REMOTE_URL, isolated_git_repository, isolated_git_repository_with,
        no_command_configuration, run_in_clean_test_process,
    };
    use super::*;
    use anyhow::{Context, Result};
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_is_git_repo() {
        run_in_clean_test_process("git_info::tests::test_is_git_repo", no_command_configuration, test_is_git_repo_body)
            .expect("run isolated Git repository test");
    }

    fn test_is_git_repo_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        assert!(is_git_repo(&fixture.repo_root), "fixture repository must be recognized as Git");
        Ok(())
    }

    #[test]
    fn test_get_git_repo_root() {
        run_in_clean_test_process(
            "git_info::tests::test_get_git_repo_root",
            no_command_configuration,
            test_get_git_repo_root_body,
        )
        .expect("run isolated Git repository test");
    }

    fn test_get_git_repo_root_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        let root = get_git_repo_root(&fixture.repo_root)?.context("fixture repository must have a Git root")?;
        assert_eq!(PathBuf::from(root), fixture.repo_root, "Git top-level path must equal the created fixture root");
        Ok(())
    }

    #[test]
    fn test_get_head_commit_hash() {
        run_in_clean_test_process(
            "git_info::tests::test_get_head_commit_hash",
            no_command_configuration,
            test_get_head_commit_hash_body,
        )
        .expect("run isolated Git repository test");
    }

    fn test_get_head_commit_hash_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        let hash = get_head_commit_hash(&fixture.repo_root)?.context("fixture repository must have a short HEAD")?;
        assert_eq!(hash, fixture.short_head, "short HEAD must match the fixture commit");
        assert!(
            hash.len() >= 7 && hash.len() <= 12,
            "fixture short HEAD must use Git's 7-to-12-character abbreviation"
        );
        assert!(
            hash.chars().all(|character| character.is_ascii_hexdigit()),
            "fixture short HEAD must contain only hexadecimal characters"
        );
        Ok(())
    }

    #[test]
    fn test_collect_git_info() {
        run_in_clean_test_process(
            "git_info::tests::test_collect_git_info",
            no_command_configuration,
            test_collect_git_info_body,
        )
        .expect("run isolated Git repository test");
    }

    fn test_collect_git_info_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        let info = collect_git_info(&fixture.repo_root)?;

        assert_eq!(
            info.head_commit.as_deref(),
            Some(fixture.short_head.as_str()),
            "collected short HEAD must match the fixture commit"
        );
        assert_eq!(
            info.repo_root.as_deref(),
            fixture.repo_root.to_str(),
            "collected repository root must match the fixture root"
        );
        assert_eq!(
            info.remotes.get("origin").map(String::as_str),
            Some(FIXTURE_REMOTE_URL),
            "collected fetch remote must match the configured fixture URL"
        );
        Ok(())
    }

    const ADVERSARIAL_TARGET_ROOT_ENV: &str = "VTCODE_GIT_INFO_ADVERSARIAL_TARGET_ROOT";
    const ADVERSARIAL_TARGET_HEAD_ENV: &str = "VTCODE_GIT_INFO_ADVERSARIAL_TARGET_HEAD";
    const ADVERSARIAL_FOREIGN_REMOTE_URL: &str = "https://example.invalid/foreign.git";
    const ADVERSARIAL_FOREIGN_COMMIT_MESSAGE: &str = "git-info foreign fixture commit";

    #[test]
    fn test_clean_reexec_ignores_git_overrides() {
        const TEST_NAME: &str = "git_info::tests::test_clean_reexec_ignores_git_overrides";

        if std::env::var_os(CLEAN_TEST_CHILD_ENV).is_some() {
            run_in_clean_test_process(
                TEST_NAME,
                no_command_configuration,
                test_clean_reexec_ignores_git_overrides_body,
            )
            .expect("run isolated Git environment test");
            return;
        }

        let target = isolated_git_repository().expect("create target Git fixture");
        let foreign = isolated_git_repository_with(ADVERSARIAL_FOREIGN_REMOTE_URL, ADVERSARIAL_FOREIGN_COMMIT_MESSAGE)
            .expect("create foreign Git fixture");
        assert_ne!(target.repo_root, foreign.repo_root, "target and foreign fixture roots must differ");
        assert_ne!(target.short_head, foreign.short_head, "target and foreign fixture HEADs must differ");
        assert_ne!(FIXTURE_REMOTE_URL, ADVERSARIAL_FOREIGN_REMOTE_URL, "target and foreign origins must differ");

        let target_root = target.repo_root.clone();
        let target_head = target.short_head.clone();
        let foreign_root = foreign.repo_root.clone();
        run_in_clean_test_process(
            TEST_NAME,
            move |command| {
                command
                    .env(ADVERSARIAL_TARGET_ROOT_ENV, target_root)
                    .env(ADVERSARIAL_TARGET_HEAD_ENV, target_head)
                    .env("GIT_DIR", foreign_root.join(".git"))
                    .env("GIT_WORK_TREE", foreign_root);
            },
            test_clean_reexec_ignores_git_overrides_body,
        )
        .expect("run isolated Git environment test");
    }

    fn test_clean_reexec_ignores_git_overrides_body() -> Result<()> {
        let target_root = std::env::var_os(ADVERSARIAL_TARGET_ROOT_ENV)
            .map(PathBuf::from)
            .context("adversarial child is missing the target fixture root")?;
        let target_head =
            std::env::var(ADVERSARIAL_TARGET_HEAD_ENV).context("adversarial child is missing the target short HEAD")?;
        let target_root_string = target_root.to_str().context("target fixture root is not UTF-8")?;
        let info = collect_git_info(&target_root)?;

        assert_eq!(
            info.repo_root.as_deref(),
            Some(target_root_string),
            "clean re-exec must query the target fixture root"
        );
        assert_eq!(
            info.head_commit.as_deref(),
            Some(target_head.as_str()),
            "clean re-exec must query the target fixture short HEAD"
        );
        assert_eq!(
            info.remotes.get("origin").map(String::as_str),
            Some(FIXTURE_REMOTE_URL),
            "clean re-exec must query the target fixture origin"
        );
        Ok(())
    }

    #[test]
    fn test_non_git_directory() {
        let temp_dir = TempDir::new().expect("create isolated non-Git directory");
        let non_git_path = temp_dir.path();

        assert!(!is_git_repo(non_git_path), "new temporary directory must not be a Git repository");
        let remotes = get_git_remote_urls(non_git_path).expect("query remotes in non-Git directory");
        assert!(remotes.is_empty(), "non-Git directory must have no remotes");
        let head = get_head_commit_hash(non_git_path).expect("query HEAD in non-Git directory");
        assert!(head.is_none(), "non-Git directory must have no HEAD commit");
        let root = get_git_repo_root(non_git_path).expect("query root in non-Git directory");
        assert!(root.is_none(), "non-Git directory must have no Git root");
    }
}
