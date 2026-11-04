//! Slash command handlers for in-chat Agent Plugins management.
//!
//! Implements `/plugin` for listing, installing, removing, validating, and
//! inspecting portable Agent Plugins within interactive chat sessions. The
//! interactive manager (TUI-only) lives in the session slash-command layer;
//! this module owns the shared command actions and text-mode execution.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use diapason_agent_plugins::{FileSystemPluginInstaller, FileSystemPluginLoader, PluginInstaller, PluginLoader};

use super::plugin_commands_parser::parse_plugin_command as parse_plugin_command_impl;

/// Plugin-related command actions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PluginCommandAction {
    /// Open the interactive plugin manager (TUI sessions).
    Interactive,
    /// Show `/plugin` help.
    Help,
    /// List installed plugins.
    List,
    /// Show details for a named plugin.
    Info { name: String },
    /// Install a plugin from a git URL or local directory.
    Add { source: String, name: Option<String> },
    /// Remove an installed plugin.
    Remove { name: String },
    /// Validate a plugin directory without installing.
    Validate { path: PathBuf },
    /// Re-discover plugin MCP providers.
    Refresh,
}

/// Result of a plugin command.
#[derive(Clone, Debug)]
pub(crate) enum PluginCommandOutcome {
    /// Command handled; display the message.
    Handled { message: String },
    /// Plugin installed; the session should refresh plugin MCP providers.
    Installed { message: String },
    /// Plugin removed; the session should refresh plugin MCP providers.
    Removed { message: String },
    /// Command failed; display the error.
    Error { message: String },
}

/// Where an installed plugin lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PluginScope {
    /// `<workspace>/.agents/plugins`
    Project,
    /// `~/.agents/plugins`
    User,
}

impl PluginScope {
    pub(crate) fn label(self) -> &'static str {
        match self {
            PluginScope::Project => "project",
            PluginScope::User => "user",
        }
    }
}

/// A single installed plugin surfaced to the interactive manager.
#[derive(Clone, Debug)]
pub(crate) struct InstalledPluginEntry {
    pub(crate) name: String,
    pub(crate) version: Option<String>,
    pub(crate) description: String,
    pub(crate) root: PathBuf,
    pub(crate) scope: PluginScope,
    pub(crate) skill_count: usize,
    pub(crate) mcp_server_count: usize,
}

pub(crate) fn parse_plugin_command(input: &str) -> Result<Option<PluginCommandAction>> {
    parse_plugin_command_impl(input)
}

/// Execute a plugin command in text mode, returning an outcome to display.
pub(crate) async fn handle_plugin_command(
    action: PluginCommandAction,
    workspace: PathBuf,
) -> Result<PluginCommandOutcome> {
    match action {
        PluginCommandAction::Help => Ok(PluginCommandOutcome::Handled { message: plugin_help_text().to_string() }),
        PluginCommandAction::List => {
            // `discover_installed_plugins` does recursive `read_dir` + manifest
            // loads; run it off the async executor. See `# Blocking` docs in
            // `src/agent/runloop/git.rs`.
            let entries = tokio::task::spawn_blocking(move || discover_installed_plugins(&workspace))
                .await
                .context("plugin discovery task panicked")?;
            Ok(PluginCommandOutcome::Handled { message: render_plugin_list(&entries) })
        }
        PluginCommandAction::Info { name } => {
            let entry = tokio::task::spawn_blocking({
                let name = name.clone();
                move || find_installed_plugin(&workspace, &name)
            })
            .await
            .context("plugin lookup task panicked")?;
            match entry {
                Some(entry) => Ok(PluginCommandOutcome::Handled { message: render_plugin_info(&entry) }),
                None => Ok(PluginCommandOutcome::Error {
                    message: format!(
                        "Plugin '{name}' is not installed. Install it with 'diapason plugins add <source>' or '/plugin add <source>'."
                    ),
                }),
            }
        }
        PluginCommandAction::Add { source, name } => {
            // `install` runs `git clone` (a blocking subprocess) and recursive
            // filesystem copies; run it off the async executor. See `# Blocking`
            // docs in `src/agent/runloop/git.rs`.
            let result = tokio::task::spawn_blocking(move || {
                let installer = FileSystemPluginInstaller::new();
                installer.install(&source, name)
            })
            .await
            .context("plugin install task panicked")?;
            match result {
                Ok(installed) => {
                    let skill_count = installed.loaded.skills.len();
                    let mcp_count = installed.loaded.mcp.as_ref().map(|m| m.servers.len()).unwrap_or(0);
                    let message = format!(
                        "Installed plugin '{}' to {}.\nValidated: {} skill(s), {} MCP server(s).\n\
                         Plugin skills load at the next session start; use '/plugin refresh' to re-discover MCP providers.",
                        installed.name,
                        installed.path.display(),
                        skill_count,
                        mcp_count
                    );
                    Ok(PluginCommandOutcome::Installed { message })
                }
                Err(e) => Ok(PluginCommandOutcome::Error { message: format!("Failed to install plugin: {e}") }),
            }
        }
        PluginCommandAction::Remove { name } => {
            let result = tokio::task::spawn_blocking({
                let name = name.clone();
                move || {
                    let installer = FileSystemPluginInstaller::new();
                    installer.remove(&name)
                }
            })
            .await
            .context("plugin remove task panicked")?;
            match result {
                Ok(()) => Ok(PluginCommandOutcome::Removed {
                    message: format!("Removed plugin '{name}'."),
                }),
                Err(e) => Ok(PluginCommandOutcome::Error { message: format!("Failed to remove plugin: {e}") }),
            }
        }
        PluginCommandAction::Validate { path } => {
            let path_for_msg = path.clone();
            let result = tokio::task::spawn_blocking(move || {
                let loader = FileSystemPluginLoader::new();
                loader.load(&path)
            })
            .await
            .context("plugin validate task panicked")?;
            match result {
                Ok(loaded) => {
                    let skill_count = loaded.skills.len();
                    let mcp_count = loaded.mcp.as_ref().map(|m| m.servers.len()).unwrap_or(0);
                    Ok(PluginCommandOutcome::Handled {
                        message: format!(
                            "Valid plugin '{}': {} skill(s), {} MCP server(s).",
                            loaded.manifest.name, skill_count, mcp_count
                        ),
                    })
                }
                Err(e) => Ok(PluginCommandOutcome::Error {
                    message: format!("Invalid plugin at {}: {e}", path_for_msg.display()),
                }),
            }
        }
        PluginCommandAction::Refresh => {
            // In interactive sessions this action is intercepted by the session
            // handler and reconfigures the live MCP runtime; this is the
            // text-mode fallback for non-session execution.
            Ok(PluginCommandOutcome::Handled {
                message: "Plugin MCP providers are re-discovered at session startup. In a TUI session, '/plugin refresh' reconfigures the live MCP manager.".to_string(),
            })
        }
        PluginCommandAction::Interactive => Ok(PluginCommandOutcome::Handled {
            message: "Interactive plugin manager is available in TUI sessions. Use /plugin in inline mode to browse and manage plugins.".to_string(),
        }),
    }
}

/// Discover installed plugins across the project and user plugin roots.
///
/// # Blocking
/// Performs recursive `read_dir` and manifest loads. Must not be called on a
/// Tokio worker thread — wrap in `spawn_blocking`.
pub(crate) fn discover_installed_plugins(workspace: &Path) -> Vec<InstalledPluginEntry> {
    discover_installed_plugins_from_roots(diapason_agent_plugins::plugin_roots_for(workspace))
}

/// Discover installed plugins from an ordered list of roots (project first,
/// then user).
///
/// # Blocking
/// Performs recursive `read_dir` and manifest loads. Must not be called on a
/// Tokio worker thread — wrap in `spawn_blocking`.
pub(crate) fn discover_installed_plugins_from_roots(roots: Vec<PathBuf>) -> Vec<InstalledPluginEntry> {
    let mut entries = Vec::new();
    for (index, root) in roots.into_iter().enumerate() {
        let scope = if index == 0 {
            PluginScope::Project
        } else {
            PluginScope::User
        };
        collect_plugins_from_root(&root, scope, &mut entries);
    }
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries
}

fn collect_plugins_from_root(root: &Path, scope: PluginScope, out: &mut Vec<InstalledPluginEntry>) {
    if !root.is_dir() {
        return;
    }
    let Ok(dir_entries) = std::fs::read_dir(root) else {
        return;
    };
    let loader = FileSystemPluginLoader::new();
    for entry in dir_entries.flatten() {
        let path = entry.path();
        if !path.is_dir() || !path.join("plugin.json").is_file() {
            continue;
        }
        let Ok(loaded) = loader.load(&path) else {
            continue;
        };
        out.push(InstalledPluginEntry {
            name: loaded.manifest.name.clone(),
            version: loaded.manifest.version.clone(),
            description: loaded.manifest.description.clone().unwrap_or_default(),
            root: path,
            scope,
            skill_count: loaded.skills.len(),
            mcp_server_count: loaded.mcp.as_ref().map(|m| m.servers.len()).unwrap_or(0),
        });
    }
}

fn find_installed_plugin(workspace: &Path, name: &str) -> Option<InstalledPluginEntry> {
    discover_installed_plugins(workspace)
        .into_iter()
        .find(|entry| entry.name == name)
}

fn render_plugin_list(entries: &[InstalledPluginEntry]) -> String {
    if entries.is_empty() {
        return "No Agent Plugins installed. Install one with 'diapason plugins add <source>' or '/plugin add <source>'."
            .to_string();
    }
    let mut output = String::from("Installed Agent Plugins:\n");
    for entry in entries {
        let version = entry.version.as_deref().unwrap_or("unknown");
        let _ = std::fmt::write(
            &mut output,
            format_args!(
                "  {} (v{}) — {} skill(s), {} MCP server(s) — {} [{}]\n",
                entry.name,
                version,
                entry.skill_count,
                entry.mcp_server_count,
                entry.root.display(),
                entry.scope.label(),
            ),
        );
    }
    output.push_str("\nUse '/plugin info <name>' for details, '/plugin remove <name>' to uninstall.");
    output
}

fn render_plugin_info(entry: &InstalledPluginEntry) -> String {
    let mut output = String::new();
    let _ = std::fmt::write(
        &mut output,
        format_args!(
            "Name: {}\nVersion: {}\nDescription: {}\nRoot: {} [{}]\n",
            entry.name,
            entry.version.as_deref().unwrap_or("unknown"),
            if entry.description.is_empty() {
                "(none)"
            } else {
                &entry.description
            },
            entry.root.display(),
            entry.scope.label(),
        ),
    );
    let _ = std::fmt::write(
        &mut output,
        format_args!("Skills: {}\nMCP servers: {}\n", entry.skill_count, entry.mcp_server_count),
    );
    output
}

fn plugin_help_text() -> &'static str {
    r#"Plugin Commands:

Interactive:
  /plugin                                Open interactive plugin manager in TUI
  /plugin manager                        Alias for interactive plugin manager

Management:
  /plugin list                           List installed plugins
  /plugin info <name>                    Show plugin details
  /plugin add <source> [--name <id>]     Install a plugin (git URL or local dir)
  /plugin remove <name>                  Uninstall a plugin
  /plugin validate <path>                Validate a plugin without installing
  /plugin refresh                        Re-discover plugin MCP providers
  /plugin help                           Show this help"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_plugin(root: &Path, name: &str) {
        let plugin_dir = root.join(name);
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(
            plugin_dir.join("plugin.json"),
            format!(
                r#"{{"$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json", "name": "{name}", "version": "1.0.0", "description": "test plugin"}}"#
            ),
        )
        .unwrap();
        std::fs::create_dir_all(plugin_dir.join("skills/demo")).unwrap();
        std::fs::write(
            plugin_dir.join("skills/demo/SKILL.md"),
            "---\nname: demo\ndescription: demo skill\n---\nDo demo.\n",
        )
        .unwrap();
    }

    #[test]
    fn discover_entries_from_roots_with_scope() {
        let project = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let project_root = project.path().join(".agents/plugins");
        let user_root = user.path().join(".agents/plugins");
        write_plugin(&project_root, "proj-plugin");
        write_plugin(&user_root, "user-plugin");

        let entries = discover_installed_plugins_from_roots(vec![project_root, user_root]);

        assert_eq!(entries.len(), 2);
        let proj = entries.iter().find(|e| e.name == "proj-plugin").expect("proj plugin");
        assert_eq!(proj.scope, PluginScope::Project);
        assert_eq!(proj.skill_count, 1);
        let user_entry = entries.iter().find(|e| e.name == "user-plugin").expect("user plugin");
        assert_eq!(user_entry.scope, PluginScope::User);
    }

    #[test]
    fn invalid_directories_are_skipped() {
        let root = TempDir::new().unwrap();
        let plugins_root = root.path().join(".agents/plugins");
        std::fs::create_dir_all(plugins_root.join("not-a-plugin")).unwrap();
        write_plugin(&plugins_root, "real-plugin");

        let entries = discover_installed_plugins_from_roots(vec![plugins_root]);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "real-plugin");
    }

    #[test]
    fn installed_plugin_names_collected() {
        let project = TempDir::new().unwrap();
        let user = TempDir::new().unwrap();
        let project_root = project.path().join(".agents/plugins");
        let user_root = user.path().join(".agents/plugins");
        write_plugin(&project_root, "a-plugin");
        write_plugin(&user_root, "b-plugin");

        let names = discover_installed_plugins_from_roots(vec![project_root, user_root])
            .into_iter()
            .map(|e| e.name)
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["a-plugin".to_string(), "b-plugin".to_string()]);
    }
}
