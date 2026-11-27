//! Core provider configuration type.

use diapason_config::TimeoutsConfig;
use diapason_config::auth::CopilotAuthConfig;
use diapason_config::auth::OpenAIChatGptAuthHandle;
use diapason_config::core::{AnthropicConfig, ModelConfig, OpenAIConfig, PromptCachingConfig};
use std::path::PathBuf;

/// Classifies a request for provider-specific admission policies.
#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub enum RequestAdmissionClass {
    /// A regular request that may use the provider's full configured capacity.
    #[default]
    Parent,
    /// A child request that shares the provider's reserved child capacity.
    Child,
}

#[derive(Debug, Clone)]
pub struct ProviderConfig {
    pub api_key: Option<String>,
    pub openai_chatgpt_auth: Option<OpenAIChatGptAuthHandle>,
    pub copilot_auth: Option<CopilotAuthConfig>,
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub prompt_cache: Option<PromptCachingConfig>,
    pub timeouts: Option<TimeoutsConfig>,
    pub openai: Option<OpenAIConfig>,
    pub anthropic: Option<AnthropicConfig>,
    pub model_behaviour: Option<ModelConfig>,
    pub workspace_root: Option<PathBuf>,
    pub request_admission_class: RequestAdmissionClass,
}
