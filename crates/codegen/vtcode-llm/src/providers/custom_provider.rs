use async_trait::async_trait;
use vtcode_commons::llm::BackendKind;
use vtcode_config::core::AnthropicConfig;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig, ResolvedCustomProviderProfile};
use vtcode_config::{ModelConfig, OpenAIConfig, PromptCachingConfig, TimeoutsConfig};

use crate::provider::{
    LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message, ResponsesCompactionOptions, SamplingOverrides,
};
use crate::providers::anthropic::{self, AnthropicProvider};
use crate::providers::common::{resolve_model, validate_request_common};
use crate::providers::openai::{CustomProviderAuthHandle, OpenAIProvider};

/// Profile-aware backend router for custom providers.
pub struct CustomProviderBackendRouter {
    provider_key: String,
    display_name: String,
    default_model: String,
    supported_models: Vec<String>,
    custom_config: CustomProviderConfig,
    anthropic_config: AnthropicConfig,
    openai_auto: OpenAIProvider,
    openai_chat: OpenAIProvider,
    openai_responses: OpenAIProvider,
    anthropic: AnthropicProvider,
}

impl CustomProviderBackendRouter {
    #[allow(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    pub fn from_config(
        custom_config: CustomProviderConfig,
        api_key: Option<String>,
        model: Option<String>,
        base_url: String,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        openai: Option<OpenAIConfig>,
        anthropic: Option<AnthropicConfig>,
        model_behaviour: Option<ModelConfig>,
        custom_provider_auth: Option<CustomProviderAuthHandle>,
    ) -> Self {
        let provider_key = custom_config.name.clone();
        let display_name = custom_config.display_name.clone();
        let supported_models = custom_config.effective_models();
        let configured_default = resolve_model(model, &custom_config.model);
        let default_model = if configured_default.trim().is_empty() {
            supported_models.first().cloned().unwrap_or_default()
        } else {
            configured_default
        };
        let anthropic_config = anthropic.unwrap_or_default();

        let openai_auto = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behaviour.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_custom_provider_config(custom_config.clone());
        let openai_chat = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behaviour.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_api_format_override(Some(CustomProviderApiFormat::OpenAIChat))
        .with_custom_provider_config(custom_config.clone());
        let openai_responses = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behaviour.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_api_format_override(Some(CustomProviderApiFormat::OpenAIResponses))
        .with_custom_provider_config(custom_config.clone());
        let anthropic = AnthropicProvider::from_config(
            api_key,
            Some(default_model.clone()),
            Some(base_url),
            prompt_cache,
            timeouts,
            Some(anthropic_config.clone()),
            model_behaviour,
        )
        .with_custom_auth(custom_provider_auth)
        .with_rate_limit_headers(custom_config.effective_rate_limit_headers());

        Self {
            provider_key,
            display_name,
            default_model,
            supported_models,
            custom_config,
            anthropic_config,
            openai_auto,
            openai_chat,
            openai_responses,
            anthropic,
        }
    }

    fn resolved_model<'a>(&'a self, model: &'a str) -> &'a str {
        if model.trim().is_empty() {
            &self.default_model
        } else {
            model
        }
    }

    fn profile_for_model(&self, model: &str) -> ResolvedCustomProviderProfile {
        self.custom_config.resolved_profile(model)
    }

    fn backend_for_model(&self, model: &str) -> &dyn LLMProvider {
        match self.profile_for_model(self.resolved_model(model)).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => &self.anthropic,
            Some(CustomProviderApiFormat::OpenAIChat) => &self.openai_chat,
            Some(CustomProviderApiFormat::OpenAIResponses) => &self.openai_responses,
            Some(CustomProviderApiFormat::Auto) | None => &self.openai_auto,
        }
    }

    fn override_bool(value: Option<bool>, default: bool) -> bool {
        value.unwrap_or(default)
    }
}

#[async_trait]
impl LLMProvider for CustomProviderBackendRouter {
    fn name(&self) -> &str {
        &self.provider_key
    }

    fn backend_kind(&self) -> BackendKind {
        self.backend_for_model(&self.default_model).backend_kind()
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_non_streaming(&self, model: &str) -> bool {
        self.backend_for_model(model).supports_non_streaming(self.resolved_model(model))
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_reasoning, self.backend_for_model(model).supports_reasoning(resolved))
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        // A pin in THIS model's own profile implies effort support unless the
        // same profile explicitly disables it. Provider-level defaults — both
        // the effort value and the capability flag — must not flip support for
        // every model on the endpoint.
        let own = self.custom_config.profiles.get(resolved);
        let own_pin = own.and_then(|p| p.reasoning_effort).is_some();
        if own_pin {
            return own.and_then(|p| p.supports_reasoning_effort) != Some(false);
        }
        Self::override_bool(
            profile.supports_reasoning_effort,
            self.backend_for_model(model).supports_reasoning_effort(resolved),
        )
    }

    fn sampling_overrides(&self, model: &str) -> SamplingOverrides {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        SamplingOverrides {
            temperature: profile.temperature,
            top_p: profile.top_p,
            top_k: profile.top_k,
            presence_penalty: profile.presence_penalty,
            frequency_penalty: profile.frequency_penalty,
            max_tokens: profile.max_tokens,
            reasoning_effort: profile.reasoning_effort,
            suppresses_sampling_with_reasoning: profile.api_format == Some(CustomProviderApiFormat::AnthropicMessages),
            profile_aware: true,
        }
    }

    fn supports_tools(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_tools, self.backend_for_model(model).supports_tools(resolved))
    }

    fn supports_parallel_tool_config(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_parallel_tool_calls,
            self.backend_for_model(model).supports_parallel_tool_config(resolved),
        )
    }

    fn supports_structured_output(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_structured_output,
            self.backend_for_model(model).supports_structured_output(resolved),
        )
    }

    fn supports_context_caching(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_caching,
            self.backend_for_model(model).supports_context_caching(resolved),
        )
    }

    fn supports_vision(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_vision, self.backend_for_model(model).supports_vision(resolved))
    }

    fn supports_responses_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        let backend_default = match self.profile_for_model(resolved).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => false,
            _ => self.backend_for_model(model).supports_responses_compaction(resolved),
        };
        Self::override_bool(profile.supports_responses_compaction, backend_default)
    }

    fn supports_native_allowed_tools(&self, model: &str) -> bool {
        self.backend_for_model(model)
            .supports_native_allowed_tools(self.resolved_model(model))
    }

    fn supports_context_edits(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_edits,
            self.backend_for_model(model).supports_context_edits(resolved),
        )
    }

    fn supports_turn_scoped_system_messages(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        matches!(self.profile_for_model(resolved).api_format, Some(CustomProviderApiFormat::AnthropicMessages))
            && self.backend_for_model(model).supports_turn_scoped_system_messages(resolved)
    }

    fn supports_manual_openai_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        let backend_default = match self.profile_for_model(resolved).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => false,
            _ => self.backend_for_model(model).supports_manual_openai_compaction(resolved),
        };
        Self::override_bool(profile.supports_responses_compaction, backend_default)
    }

    fn supports_native_inline_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_edits,
            self.backend_for_model(model).supports_native_inline_compaction(resolved),
        )
    }

    fn manual_openai_compaction_unavailable_message(&self, model: &str) -> String {
        self.backend_for_model(model)
            .manual_openai_compaction_unavailable_message(self.resolved_model(model))
    }

    fn effective_context_size(&self, model: &str) -> usize {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        profile
            .context_window
            .unwrap_or_else(|| self.backend_for_model(model).effective_context_size(resolved))
    }

    async fn compact_history(&self, model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        self.backend_for_model(model)
            .compact_history(self.resolved_model(model), history)
            .await
    }

    async fn compact_history_with_options(
        &self,
        model: &str,
        history: &[Message],
        options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.backend_for_model(model)
            .compact_history_with_options(self.resolved_model(model), history, options)
            .await
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.backend_for_model(&request.model).stream(request).await
    }

    async fn stream_normalized(&self, request: LLMRequest) -> Result<crate::provider::LLMNormalizedStream, LLMError> {
        self.backend_for_model(&request.model).stream_normalized(request).await
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.backend_for_model(&request.model).generate(request).await
    }

    fn supported_models(&self) -> Vec<String> {
        self.supported_models.clone()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        let resolved_model = self.resolved_model(&request.model);
        match self.profile_for_model(resolved_model).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => {
                // Anthropic-shaped custom profiles use the Anthropic message
                // contract, including its turn-scoped system marker. Keep the
                // custom display name for diagnostics, but validate message roles
                // against the backend contract so the marker is not rejected.
                validate_request_common(request, &self.display_name, "anthropic", Some(&self.supported_models))?;
                anthropic::validation::validate_request(
                    request,
                    &self.default_model,
                    &self.anthropic_config,
                    &self.display_name,
                )
            }
            Some(CustomProviderApiFormat::OpenAIChat)
            | Some(CustomProviderApiFormat::OpenAIResponses)
            | Some(CustomProviderApiFormat::Auto)
            | None => self.backend_for_model(resolved_model).validate_request(request),
        }
    }
}

#[cfg(test)]
mod tests;
