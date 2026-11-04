use clap::{Args, Subcommand};
use std::path::PathBuf;

/// Configuration subcommands.
#[derive(Debug, Clone, Subcommand)]
pub enum ConfigCommand {
    /// Clear one configuration layer and reload the effective settings.
    Reset(ConfigResetArgs),
}

/// Arguments for `diapason config reset`.
#[derive(Debug, Clone, Args)]
pub struct ConfigResetArgs {
    /// Clear the canonical user-level configuration layer.
    #[arg(long, conflicts_with = "project")]
    pub global: bool,
    /// Clear the current workspace's project-profile configuration layer.
    #[arg(long, conflicts_with = "global")]
    pub project: bool,
}

/// Configuration file structure with latest features
#[derive(Debug)]
pub struct ConfigFile {
    pub model: Option<String>,
    pub provider: Option<String>,
    pub api_key_env: Option<String>,
    pub verbose: Option<bool>,
    pub log_level: Option<String>,
    pub workspace: Option<PathBuf>,
    pub tools: Option<ToolConfig>,
    pub context: Option<ContextConfig>,
    pub logging: Option<LoggingConfig>,
    pub performance: Option<PerformanceConfig>,
    pub security: Option<SecurityConfig>,
}

/// Tool configuration from config file
#[derive(Debug, serde::Deserialize)]
pub struct ToolConfig {
    pub enable_validation: Option<bool>,
    pub max_execution_time_seconds: Option<u64>,
    pub allow_file_creation: Option<bool>,
    pub allow_file_deletion: Option<bool>,
}

/// Context management configuration
#[derive(Debug, serde::Deserialize)]
pub struct ContextConfig {
    pub max_context_length: Option<usize>,
}

/// Logging configuration
#[derive(Debug, serde::Deserialize)]
pub struct LoggingConfig {
    pub file_logging: Option<bool>,
    pub log_directory: Option<String>,
    pub max_log_files: Option<usize>,
    pub max_log_size_mb: Option<usize>,
}

/// Performance monitoring configuration
#[derive(Debug, serde::Deserialize)]
pub struct PerformanceConfig {
    pub enabled: Option<bool>,
    pub track_token_usage: Option<bool>,
    pub track_api_costs: Option<bool>,
    pub track_response_times: Option<bool>,
    pub enable_benchmarking: Option<bool>,
    pub metrics_retention_days: Option<usize>,
}

/// Security configuration
#[derive(Debug, serde::Deserialize)]
pub struct SecurityConfig {
    pub level: Option<String>,
    pub enable_audit_logging: Option<bool>,
    pub enable_vulnerability_scanning: Option<bool>,
    pub allow_external_urls: Option<bool>,
    pub max_file_access_depth: Option<usize>,
}
