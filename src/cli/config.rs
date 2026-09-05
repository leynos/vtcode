use anyhow::Result;
use std::path::Path;
use vtcode_core::cli::args::{ConfigCommand, ConfigResetArgs};
use vtcode_core::config::{ConfigReadRequest, ConfigResetRequest, ConfigService, ConfigWriteTarget, VTCodeConfig};
use vtcode_core::utils::colours::style;

/// Handle the config command
pub async fn handle_config_command(
    output: Option<&Path>,
    use_home_dir: bool,
    command: Option<ConfigCommand>,
    workspace: &Path,
) -> Result<()> {
    println!("{}\n", style("[CONFIG]").cyan().bold());

    if let Some(command) = command {
        if output.is_some() {
            anyhow::bail!("--output cannot be combined with a config subcommand");
        }
        match command {
            ConfigCommand::Reset(args) => return handle_reset_command(args, use_home_dir, workspace),
        }
    }

    if use_home_dir {
        // Create config in user's home directory
        let created_files = VTCodeConfig::bootstrap_project_with_options(
            std::env::current_dir()?,
            true, // force overwrite
            true, // use home directory
        )?;

        if !created_files.is_empty() {
            println!("Configuration files created in user home directory:");
            for file in created_files {
                println!("  - {file}");
            }
        } else {
            println!("Configuration files already exist in user home directory");
        }
    } else if let Some(output_path) = output {
        println!("Output path: {}", output_path.display());

        // Write to specified file (non-blocking Tokio filesystem API)
        tokio::fs::write(output_path, generate_default_config().as_bytes()).await?;
        println!("Configuration written to {}", output_path.display());
    } else {
        // Print to stdout
        println!("\nGenerated configuration:\n");
        println!("{}", generate_default_config());
    }

    Ok(())
}

fn handle_reset_command(args: ConfigResetArgs, parent_global: bool, workspace: &Path) -> Result<()> {
    if parent_global && args.project {
        anyhow::bail!("Choose only one config reset target: --global or --project");
    }

    let target = if parent_global || args.global {
        ConfigWriteTarget::User
    } else if args.project {
        ConfigWriteTarget::Project
    } else {
        ConfigWriteTarget::Workspace
    };

    let response = ConfigService::reset(ConfigResetRequest {
        workspace: workspace.to_path_buf(),
        target,
        expected_layer_version: None,
        path: None,
    })?;

    if response.had_file {
        println!("Reset {} configuration layer at {}.", target.layer_name(), response.path.display());
    } else {
        println!(
            "{} configuration layer is already empty (target: {}).",
            target.layer_name(),
            response.path.display()
        );
    }

    Ok(())
}

/// Generate default configuration content
/// This function creates a complete configuration by:
/// 1. Loading existing vtcode.toml if it exists (preserving user customizations)
/// 2. Using default values if no config exists
/// 3. Generating a complete TOML structure with all sections
fn generate_default_config() -> String {
    // Try to load existing configuration to preserve user settings
    let config = if Path::new("vtcode.toml").exists() {
        let workspace = std::env::current_dir().unwrap_or_else(|_| ".".into());
        match ConfigService::read(ConfigReadRequest { workspace, runtime_overrides: Vec::new() }) {
            Ok(response) => serde_json::from_value::<VTCodeConfig>(response.effective_config)
                .unwrap_or_else(|_| VTCodeConfig::default()),
            Err(_) => VTCodeConfig::default(),
        }
    } else {
        // Use system defaults if no config file exists
        VTCodeConfig::default()
    };

    // Generate TOML content from the loaded/created config
    toml::to_string_pretty(&config).unwrap_or_else(|_| {
        // Fallback to hardcoded template if serialization fails
        r#"# VT Code Configuration File
# This file contains the configuration for VT Code

[agent]
# Default model to use
default_model = "gpt-5.6-sol"
# AI provider (gemini, openai, anthropic, meta, openrouter, merge-gateway)
provider = "openai"
# Environment variable containing API key
api_key_env = "OPENAI_API_KEY"
# Maximum conversation turns
max_conversation_turns = 150
# Reasoning effort level for models that support it (none, minimal, low, medium, high, xhigh, max)
reasoning_effort = "none"
# Main model temperature
temperature = 0.7

[security]
# Enable human-in-the-loop mode
human_in_the_loop = true

[tools]
# Default tool execution policy
default_policy = "prompt"

[commands]
# Allowed shell commands (whitelist)
allow_list = ["ls", "pwd", "cat", "grep", "git status", "git diff"]

[pty]
# Enable PTY support
enabled = true
# Default terminal dimensions
default_rows = 24
default_cols = 80
# Maximum concurrent PTY sessions
max_sessions = 10
# Command execution timeout in seconds
command_timeout_seconds = 300
"#
        .to_string()
    })
}
