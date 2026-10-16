//! Builds the automatic, Chat Completions, and Responses OpenAI backends.
use crate::providers::openai::{CustomProviderAuthHandle, OpenAIProvider};
use std::sync::Arc;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig};
use vtcode_config::{ModelConfig, OpenAIConfig, PromptCachingConfig, TimeoutsConfig};

/// Profile-aware backend router for custom providers.
pub(super) struct CustomProviderOpenAIRequestConfig {
    pub(super) provider_key: String,
    pub(super) display_name: String,
    pub(super) api_key: Option<String>,
    pub(super) model: String,
    pub(super) base_url: String,
    pub(super) prompt_cache: Option<PromptCachingConfig>,
    pub(super) timeouts: Option<TimeoutsConfig>,
    pub(super) openai: Option<OpenAIConfig>,
    pub(super) model_behaviour: Option<ModelConfig>,
    pub(super) auth: Option<CustomProviderAuthHandle>,
    pub(super) supported_models: Vec<String>,
}

pub(super) struct CustomProviderOpenAIBackends {
    pub(super) automatic: OpenAIProvider,
    pub(super) chat: OpenAIProvider,
    pub(super) responses: OpenAIProvider,
}

impl CustomProviderOpenAIRequestConfig {
    fn backend(
        &self,
        custom_config: &CustomProviderConfig,
        api_format: Option<CustomProviderApiFormat>,
    ) -> OpenAIProvider {
        let provider = OpenAIProvider::from_custom_config(
            self.provider_key.clone(),
            self.display_name.clone(),
            self.api_key.clone(),
            Some(self.model.clone()),
            Some(self.base_url.clone()),
            self.prompt_cache.clone(),
            self.timeouts.clone(),
            self.openai.clone(),
            self.model_behaviour.clone(),
            self.auth.clone(),
            Some(self.supported_models.clone()),
        );
        let provider = match api_format {
            Some(format) => provider.with_api_format_override(Some(format)),
            None => provider,
        };
        provider.with_custom_provider_config(custom_config.clone())
    }

    pub(super) fn backends(&self, custom_config: &CustomProviderConfig) -> CustomProviderOpenAIBackends {
        CustomProviderOpenAIBackends {
            automatic: self.backend(custom_config, None),
            chat: self.backend(custom_config, Some(CustomProviderApiFormat::OpenAIChat)),
            responses: self.backend(custom_config, Some(CustomProviderApiFormat::OpenAIResponses)),
        }
    }
}
