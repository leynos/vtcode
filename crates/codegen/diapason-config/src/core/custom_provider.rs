//! Defines custom provider profiles, request policies, and their serialized configuration.
mod api_format {
    //! Typed API format selection for custom providers.
    use serde::{Deserialize, Serialize};

    pub(super) fn skip_serializing_custom_provider_api_format(api_format: &CustomProviderApiFormat) -> bool {
        api_format.is_auto()
    }

    /// Typed API format used by custom providers.
    #[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
    #[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
    pub enum CustomProviderApiFormat {
        #[default]
        #[serde(rename = "auto")]
        Auto,
        #[serde(rename = "openai-chat")]
        OpenAIChat,
        #[serde(rename = "openai-responses")]
        OpenAIResponses,
        #[serde(rename = "anthropic-messages")]
        AnthropicMessages,
    }

    impl CustomProviderApiFormat {
        pub const fn as_str(self) -> &'static str {
            match self {
                Self::Auto => "auto",
                Self::OpenAIChat => "openai-chat",
                Self::OpenAIResponses => "openai-responses",
                Self::AnthropicMessages => "anthropic-messages",
            }
        }

        pub const fn is_auto(self) -> bool {
            matches!(self, Self::Auto)
        }

        pub const fn resolved(self) -> Option<Self> {
            match self {
                Self::Auto => None,
                other => Some(other),
            }
        }
    }
}
mod auth {
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
}
mod profile;
mod provider;
mod request_policy;

pub use api_format::CustomProviderApiFormat;
pub use auth::CustomProviderCommandAuthConfig;
pub use profile::{CustomProviderProfileConfig, ResolvedCustomProviderProfile};
pub use provider::CustomProviderConfig;
pub use request_policy::CustomProviderRequestPolicyConfig;

#[cfg(test)]
mod tests {
    //! Tests for custom provider configuration parsing, resolution, and validation.

    use super::CustomProviderConfig;

    mod profile;
    mod provider_validation;
    mod request_policy;

    fn base_provider_config() -> CustomProviderConfig {
        CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            model: "gpt-5-mini".to_string(),
            ..CustomProviderConfig::default()
        }
    }
}
