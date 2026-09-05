//! CLI commands for managing tool policies

use crate::tool_policy::{ToolPolicy, ToolPolicyManager};
use crate::tools::ToolRegistry;
use crate::utils::colours::style;
use anyhow::Result;
use clap::Subcommand;

/// Tool policy management commands
#[derive(Debug, Clone, Subcommand)]
pub enum ToolPolicyCommands {
    /// Show current tool policy status
    Status,
    /// Allow a specific tool
    Allow {
        /// Tool name to allow
        tool: String,
    },
    /// Deny a specific tool
    Deny {
        /// Tool name to deny
        tool: String,
    },
    /// Set a tool to prompt for confirmation
    Prompt {
        /// Tool name to set to prompt
        tool: String,
    },
    /// Allow all tools
    AllowAll,
    /// Deny all tools
    DenyAll,
    /// Reset all tools to prompt
    ResetAll,
}

/// Handle tool policy commands
pub async fn handle_tool_policy_command(command: ToolPolicyCommands) -> Result<()> {
    let mut policy_manager = ToolPolicyManager::new().await?;

    match command {
        ToolPolicyCommands::Status => {
            policy_manager.print_status();
        }
        ToolPolicyCommands::Allow { tool } => {
            let normalized_tool = normalize_cli_tool_name(&tool).await;
            policy_manager.set_policy(&normalized_tool, ToolPolicy::Allow).await?;
            println!(
                "{}",
                style(format!("✓ Tool '{}' is now allowed", display_tool_name(&tool, &normalized_tool))).green()
            );
        }
        ToolPolicyCommands::Deny { tool } => {
            let normalized_tool = normalize_cli_tool_name(&tool).await;
            policy_manager.set_policy(&normalized_tool, ToolPolicy::Deny).await?;
            println!(
                "{}",
                style(format!("✗ Tool '{}' is now denied", display_tool_name(&tool, &normalized_tool))).red()
            );
        }
        ToolPolicyCommands::Prompt { tool } => {
            let normalized_tool = normalize_cli_tool_name(&tool).await;
            policy_manager.set_policy(&normalized_tool, ToolPolicy::Prompt).await?;
            println!(
                "{}",
                style(format!(
                    "? Tool '{}' will now prompt for confirmation",
                    display_tool_name(&tool, &normalized_tool)
                ))
                .cyan()
            );
        }
        ToolPolicyCommands::AllowAll => {
            policy_manager.allow_all_tools().await?;
            println!("{}", style("✓ All tools are now allowed").green());
        }
        ToolPolicyCommands::DenyAll => {
            policy_manager.deny_all_tools().await?;
            println!("{}", style("✗ All tools are now denied").red());
        }
        ToolPolicyCommands::ResetAll => {
            policy_manager.reset_all_to_prompt().await?;
            println!("{}", style("? All tools reset to prompt for confirmation").cyan());
        }
    }

    Ok(())
}

async fn normalize_cli_tool_name(tool: &str) -> String {
    let Ok(workspace_root) = std::env::current_dir() else {
        return tool.to_string();
    };

    let registry = ToolRegistry::new(workspace_root).await;
    registry
        .resolve_public_tool_name_sync(tool)
        .unwrap_or_else(|_| tool.to_string())
}

fn display_tool_name(requested_tool: &str, normalized_tool: &str) -> String {
    if requested_tool == normalized_tool {
        requested_tool.to_string()
    } else {
        format!("{requested_tool} -> {normalized_tool}")
    }
}
