//! Turn metadata for LLM requests
//!
//! This module provides utilities for building turn metadata headers that are
//! sent with LLM requests, similar to OpenAI Codex PR #10145. The metadata
//! includes workspace information like git remote URLs and commit hash.

use crate::git_info::{self};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

/// Workspace information included in turn metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct WorkspaceInfo {
    /// Git remote URLs keyed by remote name
    #[serde(skip_serializing_if = "BTreeMap::is_empty", default)]
    pub remote_urls: BTreeMap<String, String>,
    /// HEAD commit hash
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_hash: Option<String>,
    /// Repository root path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repo_root: Option<String>,
}

/// Turn metadata structure sent as X-Turn-Metadata header
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TurnMetadata {
    /// Workspace information
    pub workspace: WorkspaceInfo,
}

/// Build the turn metadata header value for the given working directory.
///
/// This function collects git information from the workspace and formats it
/// as a JSON string suitable for use as the X-Turn-Metadata header value.
///
/// # Arguments
/// * `cwd` - The working directory to collect metadata from
///
/// # Returns
/// A JSON string containing the turn metadata, or an empty string if
/// metadata collection fails or the directory is not in a git repository.
///
/// # Example
/// ```
/// use std::path::Path;
/// use vtcode_core::turn_metadata::build_turn_metadata_header;
///
/// let metadata = build_turn_metadata_header(Path::new(".")).unwrap();
/// // metadata will be a JSON string like:
/// // {"workspace":{"remote_urls":{"origin":"https://github.com/user/repo.git"},"commit_hash":"abc1234"}}
/// ```
pub fn build_turn_metadata_header(cwd: &Path) -> Result<String> {
    if !git_info::is_git_repo(cwd) {
        return Ok(String::new());
    }

    let git_info = git_info::collect_git_info(cwd)?;

    let metadata = TurnMetadata {
        workspace: WorkspaceInfo {
            remote_urls: git_info.remotes,
            commit_hash: git_info.head_commit,
            repo_root: git_info.repo_root,
        },
    };

    // Serialize to compact JSON
    let json = serde_json::to_string(&metadata)?;
    Ok(json)
}

/// Build turn metadata as a serde_json::Value for direct use in LLM requests.
///
/// # Arguments
/// * `cwd` - The working directory to collect metadata from
///
/// # Returns
/// A serde_json::Value containing the turn metadata, or Null if
/// metadata collection fails or the directory is not in a git repository.
pub fn build_turn_metadata_value(cwd: &Path) -> Result<Value> {
    if !git_info::is_git_repo(cwd) {
        return Ok(Value::Null);
    }

    let git_info = git_info::collect_git_info(cwd)?;

    let metadata = TurnMetadata {
        workspace: WorkspaceInfo {
            remote_urls: git_info.remotes,
            commit_hash: git_info.head_commit,
            repo_root: git_info.repo_root,
        },
    };

    let value = serde_json::to_value(metadata)?;
    Ok(value)
}

/// Build turn metadata with a timeout to avoid blocking turn execution.
///
/// Returns `Ok(None)` when metadata is unavailable or times out.
pub async fn build_turn_metadata_value_with_timeout(cwd: &Path, timeout: Duration) -> Result<Option<Value>> {
    let cwd = cwd.to_path_buf();
    let handle = tokio::task::spawn_blocking(move || build_turn_metadata_value(&cwd));
    match tokio::time::timeout(timeout, handle).await {
        Ok(join_result) => {
            let value = join_result.context("Turn metadata task failed")??;
            if value.is_null() { Ok(None) } else { Ok(Some(value)) }
        }
        Err(_) => {
            tracing::debug!("Turn metadata collection timed out");
            Ok(None)
        }
    }
}

/// Get the header name for turn metadata.
/// This is the header key used when sending metadata to LLM providers.
pub const TURN_METADATA_HEADER: &str = "X-Turn-Metadata";

#[cfg(test)]
mod tests {
    //! Verifies turn metadata serialization against controlled Git workspace state.

    use super::*;
    use crate::git_info::test_support::{
        FIXTURE_REMOTE_URL, isolated_git_repository, no_command_configuration, run_in_clean_test_process,
    };
    use anyhow::Result;

    #[test]
    fn test_build_turn_metadata_header() {
        run_in_clean_test_process(
            "turn_metadata::tests::test_build_turn_metadata_header",
            no_command_configuration,
            test_build_turn_metadata_header_body,
        )
        .expect("run isolated turn metadata test");
    }

    fn test_build_turn_metadata_header_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        let metadata = build_turn_metadata_header(&fixture.repo_root)?;
        assert!(!metadata.is_empty(), "fixture repository must produce a metadata header");
        let parsed: TurnMetadata = serde_json::from_str(&metadata)?;

        assert_eq!(
            parsed.workspace.remote_urls.get("origin").map(String::as_str),
            Some(FIXTURE_REMOTE_URL),
            "metadata header must retain the fixture origin URL"
        );
        assert_eq!(
            parsed.workspace.commit_hash.as_deref(),
            Some(fixture.short_head.as_str()),
            "metadata header must retain the fixture short HEAD"
        );
        assert_eq!(
            parsed.workspace.repo_root.as_deref(),
            fixture.repo_root.to_str(),
            "metadata header must retain the fixture repository root"
        );
        Ok(())
    }

    #[test]
    fn test_build_turn_metadata_value() {
        run_in_clean_test_process(
            "turn_metadata::tests::test_build_turn_metadata_value",
            no_command_configuration,
            test_build_turn_metadata_value_body,
        )
        .expect("run isolated turn metadata test");
    }

    fn test_build_turn_metadata_value_body() -> Result<()> {
        let fixture = isolated_git_repository()?;
        let value = build_turn_metadata_value(&fixture.repo_root)?;
        assert!(!value.is_null(), "fixture repository must produce metadata value");
        let parsed: TurnMetadata = serde_json::from_value(value)?;

        assert_eq!(
            parsed.workspace.remote_urls.get("origin").map(String::as_str),
            Some(FIXTURE_REMOTE_URL),
            "metadata value must retain the fixture origin URL"
        );
        assert_eq!(
            parsed.workspace.commit_hash.as_deref(),
            Some(fixture.short_head.as_str()),
            "metadata value must retain the fixture short HEAD"
        );
        assert_eq!(
            parsed.workspace.repo_root.as_deref(),
            fixture.repo_root.to_str(),
            "metadata value must retain the fixture repository root"
        );
        Ok(())
    }

    #[test]
    fn test_turn_metadata_header_constant() {
        assert_eq!(TURN_METADATA_HEADER, "X-Turn-Metadata", "metadata header name must remain stable");
    }

    #[test]
    fn test_workspace_info_serialization() {
        let mut remotes = BTreeMap::new();
        remotes.insert("origin".to_string(), "https://github.com/user/repo.git".to_string());

        let workspace = WorkspaceInfo {
            remote_urls: remotes,
            commit_hash: Some("abc1234".to_string()),
            repo_root: Some("/path/to/repo".to_string()),
        };

        let json = serde_json::to_string(&workspace).expect("serialize populated workspace metadata");
        assert!(json.contains("origin"), "serialized workspace must include the remote name");
        assert!(json.contains("abc1234"), "serialized workspace must include the commit hash");
        assert!(json.contains("/path/to/repo"), "serialized workspace must include the repository root");
    }

    #[test]
    fn test_empty_remotes_skipped() {
        let workspace = WorkspaceInfo {
            remote_urls: BTreeMap::new(),
            commit_hash: Some("abc1234".to_string()),
            repo_root: None,
        };

        let json = serde_json::to_string(&workspace).expect("serialize workspace without remotes");
        assert!(!json.contains("remote_urls"), "empty remote map must be omitted from serialized metadata");
        assert!(json.contains("commit_hash"), "present commit hash must remain serialized");
    }
}
