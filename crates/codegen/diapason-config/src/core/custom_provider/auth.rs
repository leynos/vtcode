//! Command-backed bearer token configuration for custom providers.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn default_auth_timeout_ms() -> u64 {
    5_000
}

fn default_auth_refresh_interval_ms() -> u64 {
    300_000
}

/// Command-backed bearer token configuration for a custom provider.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct CustomProviderCommandAuthConfig {
    /// Command to execute. Bare names are resolved via `PATH`. Command-backed
    /// auth is accepted only from trusted system/user or explicitly selected
    /// configuration; repository-controlled workspace/project values are
    /// rejected.
    pub command: String,

    /// Optional command arguments.
    #[serde(default)]
    pub args: Vec<String>,

    /// Optional working directory for the token command.
    #[serde(default)]
    pub cwd: Option<PathBuf>,

    /// Maximum time to wait for the command to complete successfully.
    #[serde(default = "default_auth_timeout_ms")]
    pub timeout_ms: u64,

    /// Maximum age for the cached token before rerunning the command.
    #[serde(default = "default_auth_refresh_interval_ms")]
    pub refresh_interval_ms: u64,
}

impl Default for CustomProviderCommandAuthConfig {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: Vec::new(),
            cwd: None,
            timeout_ms: default_auth_timeout_ms(),
            refresh_interval_ms: default_auth_refresh_interval_ms(),
        }
    }
}

impl CustomProviderCommandAuthConfig {
    pub(super) fn validate(&self, provider_name: &str) -> Result<(), String> {
        if self.command.trim().is_empty() {
            return Err(format!("custom_providers[{provider_name}]: `auth.command` must not be empty"));
        }

        if self.timeout_ms == 0 {
            return Err(format!("custom_providers[{provider_name}]: `auth.timeout_ms` must be greater than 0"));
        }

        if self.refresh_interval_ms == 0 {
            return Err(format!(
                "custom_providers[{provider_name}]: `auth.refresh_interval_ms` must be greater than 0"
            ));
        }

        Ok(())
    }
}
