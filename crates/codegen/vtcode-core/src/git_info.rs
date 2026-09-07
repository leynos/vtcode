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
pub(crate) mod test_support {
    //! Builds deterministic local Git repositories for core unit tests.

    use anyhow::{Context, Result, bail};
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};
    use tempfile::TempDir;
    use vtcode_commons::canonicalize;

    const FIXTURE_COMMIT_MESSAGE: &str = "git-info fixture commit";
    pub(crate) const FIXTURE_REMOTE_URL: &str = "https://example.invalid/vtcode.git";
    const FIXTURE_AUTHOR_NAME: &str = "VTCode Test";
    const FIXTURE_AUTHOR_EMAIL: &str = "vtcode-test@example.invalid";
    const FIXTURE_COMMIT_DATE: &str = "2000-01-02T03:04:05+00:00";

    pub(crate) struct GitFixture {
        _temp_dir: TempDir,
        pub(crate) repo_root: PathBuf,
        pub(crate) short_head: String,
    }

    fn fixture_git_command(cwd: &Path) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_CEILING_DIRECTORIES")
            .env_remove("GIT_CONFIG_PARAMETERS")
            .env_remove("GIT_CONFIG")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", cwd.join("fixture-global-git-config"))
            .env("GIT_TEMPLATE_DIR", cwd)
            .env("GIT_CONFIG_COUNT", "0")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_AUTHOR_NAME", FIXTURE_AUTHOR_NAME)
            .env("GIT_AUTHOR_EMAIL", FIXTURE_AUTHOR_EMAIL)
            .env("GIT_COMMITTER_NAME", FIXTURE_AUTHOR_NAME)
            .env("GIT_COMMITTER_EMAIL", FIXTURE_AUTHOR_EMAIL)
            .env("GIT_AUTHOR_DATE", FIXTURE_COMMIT_DATE)
            .env("GIT_COMMITTER_DATE", FIXTURE_COMMIT_DATE);
        command
    }

    fn run_fixture_git(cwd: &Path, args: &[&str]) -> Result<Output> {
        let output = fixture_git_command(cwd)
            .args(args)
            .output()
            .with_context(|| format!("run fixture git command: git {}", args.join(" ")))?;
        if !output.status.success() {
            bail!(
                "fixture git command failed (git {}): {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        Ok(output)
    }

    pub(crate) fn isolated_git_repository() -> Result<GitFixture> {
        let temp_dir = TempDir::new().context("create isolated Git fixture directory")?;
        let repo_root = temp_dir.path().join("repository");
        run_fixture_git(
            temp_dir.path(),
            &[
                "init",
                "--quiet",
                "--initial-branch=main",
                "--object-format=sha1",
                "repository",
            ],
        )?;
        let repo_root = canonicalize(repo_root).context("canonicalize isolated Git fixture root")?;
        run_fixture_git(&repo_root, &["config", "core.abbrev", "7"])?;
        run_fixture_git(&repo_root, &["remote", "add", "origin", FIXTURE_REMOTE_URL])?;
        run_fixture_git(
            &repo_root,
            &[
                "commit",
                "--quiet",
                "--allow-empty",
                "--message",
                FIXTURE_COMMIT_MESSAGE,
            ],
        )?;
        let head_output = run_fixture_git(&repo_root, &["rev-parse", "--short", "HEAD"])?;
        let short_head = String::from_utf8(head_output.stdout)
            .context("decode fixture Git short HEAD")?
            .trim()
            .to_owned();
        if short_head.is_empty() {
            bail!("fixture Git repository must have a short HEAD");
        }

        Ok(GitFixture { _temp_dir: temp_dir, repo_root, short_head })
    }
}

#[cfg(test)]
mod tests {
    //! Verifies Git information collection against hermetic local repositories.

    use super::test_support::{FIXTURE_REMOTE_URL, isolated_git_repository};
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn test_is_git_repo() {
        let fixture = isolated_git_repository().expect("create isolated Git repository fixture");
        assert!(is_git_repo(&fixture.repo_root), "fixture repository must be recognized as Git");
    }

    #[test]
    fn test_get_git_repo_root() {
        let fixture = isolated_git_repository().expect("create isolated Git repository fixture");
        let root = get_git_repo_root(&fixture.repo_root)
            .expect("collect fixture Git root")
            .expect("fixture repository must have a Git root");
        assert_eq!(PathBuf::from(root), fixture.repo_root, "Git top-level path must equal the created fixture root");
    }

    #[test]
    fn test_get_head_commit_hash() {
        let fixture = isolated_git_repository().expect("create isolated Git repository fixture");
        let hash = get_head_commit_hash(&fixture.repo_root)
            .expect("collect fixture Git short HEAD")
            .expect("fixture repository must have a short HEAD");
        assert_eq!(hash, fixture.short_head, "short HEAD must match the fixture commit");
        assert!(
            hash.len() >= 7 && hash.len() <= 12,
            "fixture short HEAD must use Git's 7-to-12-character abbreviation"
        );
        assert!(
            hash.chars().all(|character| character.is_ascii_hexdigit()),
            "fixture short HEAD must contain only hexadecimal characters"
        );
    }

    #[test]
    fn test_collect_git_info() {
        let fixture = isolated_git_repository().expect("create isolated Git repository fixture");
        let info = collect_git_info(&fixture.repo_root).expect("collect isolated fixture Git information");

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
