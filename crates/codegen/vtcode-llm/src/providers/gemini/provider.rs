use super::*;
use vtcode_config::core::ModelConfig;

pub struct GeminiProvider {
    pub(super) api_key: Arc<str>,
    pub(super) http_client: HttpClient,
    pub(super) base_url: Arc<str>,
    pub(super) model: Arc<str>,
    pub(super) prompt_cache_enabled: bool,
    pub(super) prompt_cache_settings: GeminiPromptCacheSettings,
    pub(super) timeouts: TimeoutsConfig,
    pub(super) model_behaviour: Option<ModelConfig>,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self::with_model_internal(
            api_key,
            models::google::GEMINI_3_FLASH_PREVIEW.to_string(),
            None,
            None,
            TimeoutsConfig::default(),
            None,
        )
    }

    pub fn with_model(api_key: String, model: String) -> Self {
        Self::with_model_internal(api_key, model, None, None, TimeoutsConfig::default(), None)
    }

    pub fn new_with_client(
        api_key: String,
        model: String,
        http_client: reqwest::Client,
        base_url: String,
        timeouts: TimeoutsConfig,
        prompt_cache_enabled: bool,
        prompt_cache_settings: GeminiPromptCacheSettings,
    ) -> Self {
        Self {
            api_key: Arc::from(api_key.as_str()),
            http_client,
            base_url: Arc::from(base_url.as_str()),
            model: Arc::from(model.as_str()),
            prompt_cache_enabled,
            prompt_cache_settings,
            timeouts,
            model_behaviour: None,
        }
    }

    pub fn from_config(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        _anthropic: Option<AnthropicConfig>,
        model_behaviour: Option<ModelConfig>,
    ) -> Self {
        let api_key_value = api_key.unwrap_or_default();
        let model_value = resolve_model(model, models::google::GEMINI_3_FLASH_PREVIEW);

        Self::with_model_internal(
            api_key_value,
            model_value,
            prompt_cache,
            base_url,
            timeouts.unwrap_or_default(),
            model_behaviour,
        )
    }

    fn with_model_internal(
        api_key: String,
        model: String,
        prompt_cache: Option<PromptCachingConfig>,
        base_url: Option<String>,
        timeouts: TimeoutsConfig,
        model_behaviour: Option<ModelConfig>,
    ) -> Self {
        use crate::http_client::HttpClientFactory;

        let (prompt_cache_enabled, prompt_cache_settings) = extract_prompt_cache_settings(
            prompt_cache,
            |providers| &providers.gemini,
            |cfg, provider_settings| {
                cfg.enabled && provider_settings.enabled && provider_settings.mode != GeminiPromptCacheMode::Off
            },
        );

        Self {
            api_key: Arc::from(api_key.as_str()),
            http_client: HttpClientFactory::for_llm(&timeouts),
            base_url: Arc::from(
                override_base_url(urls::GEMINI_API_BASE, base_url, Some(env_vars::GEMINI_BASE_URL)).as_str(),
            ),
            model: Arc::from(model.as_str()),
            prompt_cache_enabled,
            prompt_cache_settings,
            timeouts,
            model_behaviour,
        }
    }
}
