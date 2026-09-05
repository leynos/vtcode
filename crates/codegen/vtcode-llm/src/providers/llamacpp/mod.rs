//! llama.cpp provider: a local, OpenAI-compatible server managed by vtcode.
//!
//! Module layout (per crate-local AGENTS.md: `providers/<name>/mod.rs`):
//! - `mod.rs` (this file): `LlamaCppProvider`, provider trait impls, request
//!   model selection, public API surface.
//! - `probe.rs`: HTTP model discovery (`fetch_llamacpp_models`), `ServerProbe`,
//!   `probe_server`.
//! - `startup.rs`: model-path policy, binary/args resolution, process spawning,
//!   readiness polling.
//! - `managed.rs`: shared state registry, phase tracking, readiness
//!   orchestration (`ensure_server_ready`).

mod managed;
mod probe;
mod startup;

pub use probe::fetch_llamacpp_models;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use super::common::resolve_model;
use crate::client::LLMClient;
use crate::error_display;
use crate::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message};
use crate::providers::common::override_base_url;

use vtcode_config::TimeoutsConfig;
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::core::{AnthropicConfig, ModelConfig, PromptCachingConfig};

const LLAMACPP_CONNECTION_ERROR: &str = "llama.cpp is not responding. Install from https://llama.app and either start `llama-server -m /path/to/model.gguf --port 8080` yourself or set LLAMACPP_MODEL_PATH so VT Code can manage startup.";

pub struct LlamaCppProvider {
    inner: Box<dyn LLMProvider>,
    api_key: Option<String>,
    configured_model: Option<String>,
    model_id: String,
    base_url: String,
    prompt_cache: Option<PromptCachingConfig>,
    timeouts: Option<TimeoutsConfig>,
    anthropic: Option<AnthropicConfig>,
    model_behaviour: Option<ModelConfig>,
}

impl LlamaCppProvider {
    fn resolve_base_url(base_url: Option<String>) -> String {
        override_base_url(urls::LLAMACPP_API_BASE, base_url, Some(env_vars::LLAMACPP_BASE_URL))
    }

    fn build_inner(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        anthropic: Option<AnthropicConfig>,
        model_behaviour: Option<ModelConfig>,
    ) -> (Box<dyn LLMProvider>, String) {
        let resolved_model = resolve_model(model, models::llamacpp::DEFAULT_MODEL);
        let resolved_base = Self::resolve_base_url(base_url);
        let inner = Box::new(crate::providers::OpenAIProvider::from_config(
            api_key,
            None,
            Some(resolved_model.clone()),
            Some(resolved_base),
            prompt_cache,
            timeouts,
            anthropic,
            None,
            model_behaviour,
        ));
        (inner, resolved_model)
    }

    fn should_replace_request_model(&self, request_model: &str, discovered_models: &[String]) -> bool {
        let trimmed = request_model.trim();
        if trimmed.is_empty() || looks_like_local_model_path(trimmed) {
            return true;
        }

        if discovered_models.len() == 1 {
            let configured = self.configured_model.as_deref().map(str::trim).unwrap_or_default();
            if trimmed == models::llamacpp::DEFAULT_MODEL || trimmed == configured {
                return true;
            }
        }

        !discovered_models.iter().any(|model| model == trimmed) && discovered_models.len() == 1
    }

    fn request_model_or_default(&self, request_model: &str) -> String {
        let trimmed = request_model.trim();
        if trimmed.is_empty() {
            resolve_model(self.configured_model.clone(), models::llamacpp::DEFAULT_MODEL)
        } else {
            trimmed.to_string()
        }
    }

    fn build_request_provider(&self, model: String) -> Box<dyn LLMProvider> {
        Self::build_inner(
            self.api_key.clone(),
            Some(model),
            Some(self.base_url.clone()),
            self.prompt_cache.clone(),
            self.timeouts.clone(),
            self.anthropic.clone(),
            self.model_behaviour.clone(),
        )
        .0
    }

    async fn prepare_request(&self, mut request: LLMRequest) -> Result<(Box<dyn LLMProvider>, LLMRequest), LLMError> {
        let discovered_model = managed::ensure_server_ready(&self.base_url, self.configured_model.as_deref()).await?;
        let discovered_models = vec![discovered_model.clone()];

        if self.should_replace_request_model(&request.model, &discovered_models) || request.model.trim().is_empty() {
            request.model = discovered_model.clone();
        } else {
            request.model = self.request_model_or_default(&request.model);
        }

        Ok((self.build_request_provider(request.model.clone()), request))
    }

    pub fn from_config(
        api_key: Option<String>,
        model: Option<String>,
        base_url: Option<String>,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        anthropic: Option<AnthropicConfig>,
        model_behaviour: Option<ModelConfig>,
    ) -> Self {
        let resolved_base_url = Self::resolve_base_url(base_url.clone());
        let (inner, model_id) = Self::build_inner(
            api_key.clone(),
            model.clone(),
            base_url,
            prompt_cache.clone(),
            timeouts.clone(),
            anthropic.clone(),
            model_behaviour.clone(),
        );
        Self {
            inner,
            api_key,
            configured_model: model,
            model_id,
            base_url: resolved_base_url,
            prompt_cache,
            timeouts,
            anthropic,
            model_behaviour,
        }
    }
}

#[async_trait]
impl LLMProvider for LlamaCppProvider {
    fn name(&self) -> &str {
        "llamacpp"
    }

    fn supports_streaming(&self) -> bool {
        self.inner.supports_streaming()
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        self.inner.supports_reasoning(model)
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        self.inner.supports_reasoning_effort(model)
    }

    fn supports_tools(&self, model: &str) -> bool {
        self.inner.supports_tools(model)
    }

    fn supports_parallel_tool_config(&self, model: &str) -> bool {
        self.inner.supports_parallel_tool_config(model)
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let (provider, request) = self.prepare_request(request).await?;
        provider.generate(request).await
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        let (provider, request) = self.prepare_request(request).await?;
        provider.stream(request).await
    }

    fn supported_models(&self) -> Vec<String> {
        models::llamacpp::SUPPORTED_MODELS
            .iter()
            .map(|model| model.to_string())
            .collect()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        if request.messages.is_empty() {
            let formatted_error = error_display::format_llm_error("llama.cpp", "Messages cannot be empty");
            return Err(LLMError::InvalidRequest { message: formatted_error, metadata: None });
        }

        for message in request.messages.iter() {
            if let Err(err) = message.validate_for_provider("openai") {
                let formatted = error_display::format_llm_error("llama.cpp", &err);
                return Err(LLMError::InvalidRequest { message: formatted, metadata: None });
            }
        }

        Ok(())
    }
}

#[async_trait]
impl LLMClient for LlamaCppProvider {
    async fn generate(&mut self, prompt: &str) -> Result<LLMResponse, LLMError> {
        LLMProvider::generate(
            self,
            LLMRequest {
                messages: Arc::new(vec![Message::user(prompt.to_string())]),
                model: self
                    .configured_model
                    .clone()
                    .unwrap_or_else(|| models::llamacpp::DEFAULT_MODEL.to_string()),
                ..Default::default()
            },
        )
        .await
    }

    fn model_id(&self) -> &str {
        &self.model_id
    }
}

// Re-export the local-model-path heuristic at module scope so `startup.rs`
// can call it as `super::looks_like_local_model_path` without going through
// the provider type. This keeps the policy decision owned by the provider
// module while letting the startup seam stay stateless.
pub(super) fn looks_like_local_model_path(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return false;
    }

    trimmed.ends_with(".gguf")
        || trimmed.contains(std::path::MAIN_SEPARATOR)
        || trimmed.contains('/')
        || trimmed.starts_with('.')
        || Path::new(trimmed).exists()
}

#[cfg(test)]
mod tests {
    use super::looks_like_local_model_path;

    #[test]
    fn empty_input_is_not_a_path() {
        assert!(!looks_like_local_model_path(""));
        assert!(!looks_like_local_model_path("   "));
    }

    #[test]
    fn gguf_extension_is_local() {
        assert!(looks_like_local_model_path("qwen.gguf"));
        // The check is case-sensitive: uppercase .GGUF is not recognized as a
        // model path on its own (it has no separator and likely does not exist
        // on disk). This documents the existing behaviour, not a desired design.
        assert!(!looks_like_local_model_path("QWEN.GGUF"));
    }

    #[test]
    fn paths_with_separator_are_local() {
        assert!(looks_like_local_model_path("/abs/path/model.gguf"));
        assert!(looks_like_local_model_path("models/qwen"));
        assert!(looks_like_local_model_path("./relative/model"));
    }

    #[test]
    fn leading_dot_is_local() {
        assert!(looks_like_local_model_path("./model"));
        assert!(looks_like_local_model_path(".hidden"));
    }

    #[test]
    fn bare_model_ids_are_not_local() {
        assert!(!looks_like_local_model_path("qwen2.5-7b"));
        assert!(!looks_like_local_model_path("gpt-4o-mini"));
    }

    #[test]
    fn existing_filesystem_path_is_local() {
        // Cargo.toml always exists relative to the manifest dir.
        let path = env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml";
        assert!(looks_like_local_model_path(&path));
    }
}
