//! Dynamic Context Discovery Initialization
//!
//! This module handles initialization of dynamic context discovery directories
//! and index files at agent startup.

use anyhow::Result;
use std::path::Path;
use tokio::fs;
use tracing::{debug, trace, warn};

/// Directory structure for dynamic context discovery
pub struct DynamicContextDirs {
    /// Root configured workspace data directory
    pub diapason_dir: std::path::PathBuf,
    /// Tool output spool directory
    pub tool_outputs: std::path::PathBuf,
    /// Conversation history directory under workspace-specific XDG state
    pub history: std::path::PathBuf,
    /// MCP tools directory
    pub mcp_tools: std::path::PathBuf,
    /// Terminal sessions directory
    pub terminals: std::path::PathBuf,
    /// Skills directory
    pub skills: std::path::PathBuf,
}

impl DynamicContextDirs {
    /// Create directory structure from workspace root
    pub fn from_workspace(workspace: &Path) -> Result<Self> {
        let diapason_dir = diapason_commons::workspace_data_dir(workspace)?;
        let workspace_state = diapason_commons::workspace_state_dir(workspace)?;
        Ok(Self {
            tool_outputs: diapason_dir.join("context").join("tool_outputs"),
            history: workspace_state.join("history"),
            mcp_tools: diapason_dir.join("mcp").join("tools"),
            terminals: diapason_dir.join("terminals"),
            skills: diapason_dir.join("skills"),
            diapason_dir,
        })
    }

    /// Get directories that should exist at startup.
    pub fn startup_dirs(&self) -> Vec<&std::path::PathBuf> {
        vec![&self.tool_outputs, &self.history, &self.terminals]
    }
}

/// Initialize dynamic context discovery directories and index files
///
/// This should be called at agent startup when dynamic context is enabled.
pub async fn initialize_dynamic_context(
    workspace: &Path,
    config: &diapason_config::DynamicContextConfig,
) -> Result<DynamicContextDirs> {
    if !config.enabled {
        debug!("Dynamic context discovery is disabled, skipping initialization");
        return DynamicContextDirs::from_workspace(workspace);
    }

    let dirs = DynamicContextDirs::from_workspace(workspace)?;

    // Create startup directories only; MCP/skills directories are created on demand.
    for dir in dirs.startup_dirs() {
        if let Err(e) = fs::create_dir_all(dir).await {
            warn!(
                path = %dir.display(),
                error = %e,
                "Failed to create dynamic context directory"
            );
        } else {
            trace!(path = %dir.display(), "Created dynamic context directory");
        }
    }

    // Create README in .diapason explaining the directory structure
    let readme_path = dirs.diapason_dir.join("README.md");
    if !fs::try_exists(&readme_path).await.unwrap_or(false) {
        let readme_content = generate_diapason_readme();
        if let Err(e) = fs::write(&readme_path, &readme_content).await {
            warn!(error = %e, "Failed to create .diapason/README.md");
        }
    }

    if config.sync_terminals {
        create_initial_terminals_index(&dirs.terminals).await;
    }

    debug!(
        workspace = %workspace.display(),
        "Initialized dynamic context discovery directories"
    );

    Ok(dirs)
}

/// Ensure MCP dynamic-context directories exist once MCP is activated.
pub async fn ensure_mcp_dynamic_context(
    workspace: &Path,
    config: &diapason_config::DynamicContextConfig,
) -> Result<()> {
    if !config.enabled || !config.sync_mcp_tools {
        return Ok(());
    }

    let dirs = DynamicContextDirs::from_workspace(workspace)?;
    if let Err(e) = fs::create_dir_all(&dirs.mcp_tools).await {
        warn!(
            path = %dirs.mcp_tools.display(),
            error = %e,
            "Failed to create MCP dynamic context directory"
        );
        return Ok(());
    }
    create_initial_mcp_index(&dirs.mcp_tools).await;
    Ok(())
}

/// Ensure skills dynamic-context directories exist once skills are activated.
pub async fn ensure_skills_dynamic_context(
    workspace: &Path,
    config: &diapason_config::DynamicContextConfig,
) -> Result<()> {
    if !config.enabled || !config.sync_skills {
        return Ok(());
    }

    let dirs = DynamicContextDirs::from_workspace(workspace)?;
    if let Err(e) = fs::create_dir_all(&dirs.skills).await {
        warn!(
            path = %dirs.skills.display(),
            error = %e,
            "Failed to create skills dynamic context directory"
        );
        return Ok(());
    }
    create_initial_skills_index(&dirs.skills).await;
    Ok(())
}

/// Generate README content for .diapason directory
fn generate_diapason_readme() -> String {
    r#"# Diapason Dynamic Context Directory

This directory contains dynamic context files for Diapason agent operations.

## Directory Structure

```
.diapason/
  context/
    tool_outputs/     # Large tool outputs spooled to files
  mcp/
    tools/            # MCP tool descriptions and schemas
    status.json       # MCP provider status
  skills/
    INDEX.md          # Available skills index
    {skill_name}/     # Individual skill directories
  terminals/
    INDEX.md          # Terminal sessions index
    {session_id}.txt  # Terminal session output
```

Conversation history is stored under the workspace-specific Diapason XDG state directory.

## Purpose

These files implement **dynamic context discovery** - a pattern where large outputs
are written to files instead of being truncated. This allows the agent to:

1. Inspect full tool-output files on demand with `exec_command.cmd` using `sed`, `cat`, or `rg`
2. Search code with advanced `code_search`, and search spooled text with `exec_command.cmd` plus `rg`
3. Recover conversation details lost during summarization
4. Discover available skills and MCP tools efficiently

## Configuration

Configure in `config.toml`:

```toml
[context.dynamic]
enabled = true
tool_output_threshold = 8192  # Bytes before spooling
sync_terminals = true
persist_history = true
sync_mcp_tools = true
sync_skills = true
```

---
*This directory is managed by Diapason. Files may be automatically created, updated, or cleaned up.*
"#
    .to_string()
}

/// Create initial skills INDEX.md
async fn create_initial_skills_index(skills_dir: &Path) {
    let index_path = skills_dir.join("INDEX.md");
    if fs::try_exists(&index_path).await.unwrap_or(false) {
        return;
    }

    let content = r#"# Skills Index

This file lists all available skills for dynamic discovery.
Use `exec_command.cmd` with `find`, `sed`, or `cat` on individual skill directories for full documentation.

*No skills available yet.*

Create skills using the `save_skill` tool.

---
*Generated automatically. Do not edit manually.*
"#;

    if let Err(e) = fs::write(&index_path, content).await {
        warn!(error = %e, "Failed to create initial skills index");
    }
}

/// Create initial terminals INDEX.md
async fn create_initial_terminals_index(terminals_dir: &Path) {
    let index_path = terminals_dir.join("INDEX.md");
    if fs::try_exists(&index_path).await.unwrap_or(false) {
        return;
    }

    let content = r#"# Terminal Sessions Index

This file lists all active terminal sessions for dynamic discovery.
Use `exec_command.cmd` with `sed`, `cat`, or `rg` on individual session files for full output.

*No active terminal sessions.*

---
*Generated automatically. Do not edit manually.*
"#;

    if let Err(e) = fs::write(&index_path, content).await {
        warn!(error = %e, "Failed to create initial terminals index");
    }
}

/// Create initial MCP tools INDEX.md
async fn create_initial_mcp_index(mcp_tools_dir: &Path) {
    let index_path = mcp_tools_dir.join("INDEX.md");
    if fs::try_exists(&index_path).await.unwrap_or(false) {
        return;
    }

    let content = r#"# MCP Tools Index

This file lists all available MCP tools for dynamic discovery.
Use `exec_command.cmd` with `sed`, `cat`, or `rg` on individual tool files for full schema details.

*No MCP tools available.*

Configure MCP servers in `config.toml` or `.mcp.json`.

---
*Generated automatically. Do not edit manually.*
"#;

    if let Err(e) = fs::write(&index_path, content).await {
        warn!(error = %e, "Failed to create initial MCP tools index");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_initialize_dynamic_context() {
        let temp = tempdir().unwrap();
        let config = diapason_config::DynamicContextConfig::default();

        let dirs = initialize_dynamic_context(temp.path(), &config).await.unwrap();

        assert!(dirs.diapason_dir.exists());
        assert!(dirs.tool_outputs.exists());
        assert!(dirs.history.exists());
        assert!(dirs.terminals.exists());
        assert!(!dirs.skills.exists());
        assert!(!dirs.mcp_tools.exists());

        // Check README was created
        assert!(dirs.diapason_dir.join("README.md").exists());
        let readme = fs::read_to_string(dirs.diapason_dir.join("README.md")).await.unwrap();
        assert!(readme.contains("`exec_command.cmd`"));
        assert!(readme.contains("`code_search`"));

        // Check startup index files were created
        assert!(dirs.terminals.join("INDEX.md").exists());
        let terminals_index = fs::read_to_string(dirs.terminals.join("INDEX.md")).await.unwrap();
        assert!(terminals_index.contains("`exec_command.cmd` with `sed`, `cat`, or `rg`"));
        assert!(!dirs.skills.join("INDEX.md").exists());
        assert!(!dirs.mcp_tools.join("INDEX.md").exists());
    }

    #[test]
    fn workspace_history_uses_xdg_state_while_dynamic_files_use_workspace_data() {
        let temp = tempdir().expect("workspace");
        let dirs = DynamicContextDirs::from_workspace(temp.path()).expect("dynamic context dirs");
        let state = diapason_commons::workspace_state_dir(temp.path()).expect("workspace state");
        let data = diapason_commons::workspace_data_dir(temp.path()).expect("workspace data");

        assert_eq!(dirs.history, state.join("history"));
        assert_eq!(dirs.diapason_dir, data);
        assert!(dirs.tool_outputs.starts_with(&data));
    }

    #[tokio::test]
    async fn test_disabled_skips_creation() {
        let temp = tempdir().unwrap();
        let config = diapason_config::DynamicContextConfig { enabled: false, ..Default::default() };

        let dirs = initialize_dynamic_context(temp.path(), &config).await.unwrap();

        // Directories should not be created when disabled
        assert!(!dirs.diapason_dir.exists());
    }

    #[tokio::test]
    async fn test_ensure_mcp_dynamic_context() {
        let temp = tempdir().unwrap();
        let config = diapason_config::DynamicContextConfig::default();

        ensure_mcp_dynamic_context(temp.path(), &config).await.unwrap();

        let dirs = DynamicContextDirs::from_workspace(temp.path()).expect("dynamic context dirs");
        assert!(dirs.mcp_tools.exists());
        assert!(dirs.mcp_tools.join("INDEX.md").exists());
        let index = fs::read_to_string(dirs.mcp_tools.join("INDEX.md")).await.unwrap();
        assert!(index.contains("`exec_command.cmd` with `sed`, `cat`, or `rg`"));
    }

    #[tokio::test]
    async fn test_ensure_skills_dynamic_context() {
        let temp = tempdir().unwrap();
        let config = diapason_config::DynamicContextConfig::default();

        ensure_skills_dynamic_context(temp.path(), &config).await.unwrap();

        let dirs = DynamicContextDirs::from_workspace(temp.path()).expect("dynamic context dirs");
        assert!(dirs.skills.exists());
        assert!(dirs.skills.join("INDEX.md").exists());
        let index = fs::read_to_string(dirs.skills.join("INDEX.md")).await.unwrap();
        assert!(index.contains("`exec_command.cmd` with `find`, `sed`, or `cat`"));
    }
}
