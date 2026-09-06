use std::borrow::Cow;
use std::str::FromStr;

use crate::provider::Usage;
use vtcode_config::api_keys::api_key_env_var;
use vtcode_config::auth::AuthCredentialsStoreMode;
use vtcode_config::models::{
    ModelCatalogueEntry, ModelId, ModelPricing, Provider, ProviderModelSupport, catalogue_provider_keys,
    model_catalogue_entry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAvailability {
    Available,
    MissingCredential,
    ManagedAuthAvailable,
    Misconfigured,
    LocalOnly,
}

impl ModelAvailability {
    pub fn requires_api_key(&self) -> bool {
        matches!(self, Self::MissingCredential | Self::Misconfigured)
    }

    pub fn uses_managed_auth(&self) -> bool {
        matches!(self, Self::ManagedAuthAvailable)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicModelMeta {
    pub display_name: String,
    pub description: Option<String>,
    pub context_window: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
pub struct DynamicModelRef<'a> {
    pub provider: Provider,
    pub model_id: &'a str,
}

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub provider: Provider,
    pub model_id: String,
    pub api_key_env: String,
    pub catalogue: Option<ModelCatalogueEntry>,
    pub dynamic: Option<DynamicModelMeta>,
    pub availability: ModelAvailability,
}

impl ResolvedModel {
    pub fn known_model(&self) -> bool {
        self.catalogue.is_some()
    }

    pub fn reasoning_supported(&self) -> bool {
        self.catalogue
            .map(|entry| entry.reasoning)
            .unwrap_or_else(|| self.provider.supports_reasoning_effort(&self.model_id))
    }

    pub fn service_tier_supported(&self) -> bool {
        self.provider.supports_service_tier(&self.model_id)
    }

    pub fn supports_tool_calls(&self) -> bool {
        self.catalogue.map(|entry| entry.tool_call).unwrap_or(true)
    }

    pub fn context_window(&self) -> Option<usize> {
        self.catalogue
            .map(|entry| entry.context_window)
            .filter(|value| *value > 0)
            .or_else(|| self.dynamic.as_ref().and_then(|dynamic| dynamic.context_window))
    }

    pub fn input_modalities(&self) -> &'static [&'static str] {
        self.catalogue.map(|entry| entry.input_modalities).unwrap_or(&[])
    }

    pub fn display_name(&self) -> Cow<'_, str> {
        if let Some(catalogue) = self.catalogue {
            return Cow::Borrowed(catalogue.display_name);
        }
        if let Some(dynamic) = &self.dynamic {
            return Cow::Borrowed(dynamic.display_name.as_str());
        }
        Cow::Borrowed(self.model_id.as_str())
    }

    pub fn description(&self) -> Option<Cow<'_, str>> {
        if let Some(catalogue) = self.catalogue {
            return (!catalogue.description.is_empty()).then_some(Cow::Borrowed(catalogue.description));
        }
        self.dynamic.as_ref().and_then(|dynamic| {
            dynamic
                .description
                .as_deref()
                .filter(|value| !value.is_empty())
                .map(Cow::Borrowed)
        })
    }

    pub fn pricing(&self) -> Option<ModelPricing> {
        self.catalogue.map(|entry| entry.pricing).filter(|pricing| {
            pricing.input.is_some()
                || pricing.output.is_some()
                || pricing.cache_read.is_some()
                || pricing.cache_write.is_some()
        })
    }

    pub fn env_key(&self) -> String {
        self.api_key_env.clone()
    }
}

pub struct ModelResolver;

impl ModelResolver {
    pub fn resolve(
        provider_override: Option<&str>,
        model: &str,
        dynamic_models: &[DynamicModelRef<'_>],
        dynamic_meta: Option<DynamicModelMeta>,
    ) -> Option<ResolvedModel> {
        Self::resolve_with_mode(
            provider_override,
            model,
            dynamic_models,
            dynamic_meta,
            AuthCredentialsStoreMode::default(),
        )
    }

    /// Resolve a model using the caller's configured credential backend.
    ///
    /// Keeping the storage mode explicit at this boundary prevents model
    /// availability from disagreeing with runtime authentication when a
    /// workspace overrides the platform default.
    pub fn resolve_with_mode(
        provider_override: Option<&str>,
        model: &str,
        dynamic_models: &[DynamicModelRef<'_>],
        dynamic_meta: Option<DynamicModelMeta>,
        storage_mode: AuthCredentialsStoreMode,
    ) -> Option<ResolvedModel> {
        Self::resolve_with_mode_and_api_key_env(
            provider_override,
            model,
            dynamic_models,
            dynamic_meta,
            None,
            storage_mode,
        )
    }

    /// Resolve a model while carrying an optional provider-specific API-key
    /// environment override through availability and picker metadata.
    pub fn resolve_with_mode_and_api_key_env(
        provider_override: Option<&str>,
        model: &str,
        dynamic_models: &[DynamicModelRef<'_>],
        dynamic_meta: Option<DynamicModelMeta>,
        api_key_env: Option<&str>,
        storage_mode: AuthCredentialsStoreMode,
    ) -> Option<ResolvedModel> {
        let model = model.trim();
        if model.is_empty() {
            return None;
        }

        if let Some(provider) = provider_override.and_then(parse_provider_override) {
            return Some(Self::resolve_for_provider(
                provider,
                model,
                dynamic_models,
                dynamic_meta,
                api_key_env,
                storage_mode,
            ));
        }

        if let Ok(model_id) = ModelId::from_str(model) {
            return Some(Self::resolve_for_model_id(
                model,
                model_id,
                dynamic_models,
                dynamic_meta,
                api_key_env,
                storage_mode,
            ));
        }

        if let Some((provider, entry)) = find_catalogue_provider(model) {
            return Some(ResolvedModel {
                provider,
                model_id: model.to_string(),
                api_key_env: resolved_api_key_env(provider, api_key_env),
                catalogue: Some(entry),
                dynamic: dynamic_meta,
                availability: Self::availability_with_key(provider, model, api_key_env, storage_mode),
            });
        }

        if let Some(provider) = find_dynamic_provider(model, dynamic_models) {
            return Some(Self::resolve_for_provider(
                provider,
                model,
                dynamic_models,
                dynamic_meta,
                api_key_env,
                storage_mode,
            ));
        }

        let provider = heuristic_provider_from_model(model)?;
        Some(Self::resolve_for_provider(provider, model, dynamic_models, dynamic_meta, api_key_env, storage_mode))
    }

    pub fn resolve_provider(
        provider_override: Option<&str>,
        model: &str,
        dynamic_models: &[DynamicModelRef<'_>],
    ) -> Option<Provider> {
        Self::resolve(provider_override, model, dynamic_models, None).map(|resolved| resolved.provider)
    }

    pub fn availability(provider: Provider, model: &str) -> ModelAvailability {
        Self::availability_with_mode(provider, model, AuthCredentialsStoreMode::default())
    }

    /// Determine model availability using an explicit credential backend.
    pub fn availability_with_mode(
        provider: Provider,
        model: &str,
        storage_mode: AuthCredentialsStoreMode,
    ) -> ModelAvailability {
        Self::availability_with_key(provider, model, None, storage_mode)
    }

    /// Determine availability using a provider-specific credential key name.
    pub fn availability_with_key(
        provider: Provider,
        model: &str,
        api_key_env: Option<&str>,
        storage_mode: AuthCredentialsStoreMode,
    ) -> ModelAvailability {
        if provider.is_local() && !local_model_requires_remote_auth(provider, model) {
            return ModelAvailability::LocalOnly;
        }

        if provider.uses_managed_auth() {
            return ModelAvailability::ManagedAuthAvailable;
        }

        let env_key = resolved_api_key_env(provider, api_key_env);

        if env_key.trim().is_empty() {
            return ModelAvailability::ManagedAuthAvailable;
        }

        match vtcode_config::api_keys::resolve_credential_with_mode(provider.as_ref(), &env_key, None, storage_mode) {
            Ok(Some(resolved)) => {
                if matches!(resolved.source, vtcode_config::api_keys::CredentialSource::OAuth) {
                    return ModelAvailability::ManagedAuthAvailable;
                }
                if resolved.secret.is_some() {
                    return ModelAvailability::Available;
                }
            }
            Ok(None) => {}
            Err(_) => return ModelAvailability::Misconfigured,
        }

        if std::env::var(&env_key).is_ok() {
            return ModelAvailability::Misconfigured;
        }

        ModelAvailability::MissingCredential
    }

    fn estimate_cost(pricing: ModelPricing, usage: &Usage) -> Option<f64> {
        let input_cost = pricing.input?;
        let output_cost = pricing.output?;

        let mut total = (usage.prompt_tokens as f64 * input_cost) + (usage.completion_tokens as f64 * output_cost);

        if let Some(cache_read_cost) = pricing.cache_read {
            total += usage.cache_read_tokens_or_fallback() as f64 * cache_read_cost;
        }

        if let Some(cache_write_cost) = pricing.cache_write {
            total += usage.cache_creation_tokens_or_zero() as f64 * cache_write_cost;
        }

        Some(total)
    }

    fn resolve_for_provider(
        provider: Provider,
        model: &str,
        dynamic_models: &[DynamicModelRef<'_>],
        dynamic_meta: Option<DynamicModelMeta>,
        api_key_env: Option<&str>,
        storage_mode: AuthCredentialsStoreMode,
    ) -> ResolvedModel {
        let catalogue = model_catalogue_entry(provider.as_ref(), model);
        let dynamic = if catalogue.is_some() || !has_dynamic_model(provider, model, dynamic_models) {
            None
        } else {
            dynamic_meta.or_else(|| {
                Some(DynamicModelMeta {
                    display_name: model.to_string(),
                    description: None,
                    context_window: None,
                })
            })
        };

        ResolvedModel {
            provider,
            model_id: model.to_string(),
            api_key_env: resolved_api_key_env(provider, api_key_env),
            catalogue,
            dynamic,
            availability: Self::availability_with_key(provider, model, api_key_env, storage_mode),
        }
    }

    fn resolve_for_model_id(
        requested_model: &str,
        model_id: ModelId,
        dynamic_models: &[DynamicModelRef<'_>],
        dynamic_meta: Option<DynamicModelMeta>,
        api_key_env: Option<&str>,
        storage_mode: AuthCredentialsStoreMode,
    ) -> ResolvedModel {
        let provider = model_id.provider();
        let catalogue = model_catalogue_entry(provider.as_ref(), &model_id.as_str());
        let dynamic = if catalogue.is_some() || !has_dynamic_model(provider, requested_model, dynamic_models) {
            None
        } else {
            dynamic_meta.or_else(|| {
                Some(DynamicModelMeta {
                    display_name: requested_model.to_string(),
                    description: None,
                    context_window: None,
                })
            })
        };

        ResolvedModel {
            provider,
            model_id: requested_model.to_string(),
            api_key_env: resolved_api_key_env(provider, api_key_env),
            catalogue,
            dynamic,
            availability: Self::availability_with_key(provider, requested_model, api_key_env, storage_mode),
        }
    }
}

fn parse_provider_override(value: &str) -> Option<Provider> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Provider::from_str(trimmed).ok()
    }
}

fn find_catalogue_provider(model: &str) -> Option<(Provider, ModelCatalogueEntry)> {
    let mut matches: Vec<(Provider, ModelCatalogueEntry)> = catalogue_provider_keys()
        .iter()
        .filter_map(|provider_key| {
            let provider = Provider::from_str(provider_key).ok()?;
            model_catalogue_entry(provider_key, model).map(|entry| (provider, entry))
        })
        .collect();
    matches.sort_by_key(|(provider, _)| provider_precedence(*provider));
    matches.into_iter().next()
}

fn find_dynamic_provider(model: &str, dynamic_models: &[DynamicModelRef<'_>]) -> Option<Provider> {
    let mut matches = dynamic_models
        .iter()
        .filter(|candidate| candidate.model_id.eq_ignore_ascii_case(model))
        .map(|candidate| candidate.provider);
    let first = matches.next()?;
    if matches.all(|provider| provider == first) {
        Some(first)
    } else {
        None
    }
}

fn has_dynamic_model(provider: Provider, model: &str, dynamic_models: &[DynamicModelRef<'_>]) -> bool {
    dynamic_models
        .iter()
        .any(|candidate| candidate.provider == provider && candidate.model_id.eq_ignore_ascii_case(model))
}

fn provider_precedence(provider: Provider) -> usize {
    match provider {
        Provider::OpenAI => 0,
        Provider::Anthropic => 1,
        Provider::Gemini => 2,
        Provider::DeepSeek => 3,
        Provider::ZAI => 4,
        Provider::Minimax => 5,
        Provider::Mistral => 6,
        Provider::Moonshot => 7,
        Provider::Meta => 8,
        Provider::OpenRouter => 9,
        Provider::HuggingFace => 10,
        Provider::Copilot => 11,
        Provider::Ollama => 12,
        Provider::OllamaCloud => 13,
        Provider::LmStudio => 14,
        Provider::LlamaCpp => 15,
        Provider::OpenCodeZen => 16,
        Provider::OpenCodeGo => 17,
        Provider::MiMo => 18,
        Provider::Qwen => 19,
        Provider::StepFun => 20,
        Provider::Evolink => 21,
        Provider::Poolside => 22,
        Provider::XAI => 23,
        Provider::NVIDIA => 24,
        Provider::MergeGateway => 25,
    }
}

fn local_model_requires_remote_auth(provider: Provider, model: &str) -> bool {
    provider == Provider::OllamaCloud
        || (provider == Provider::Ollama && (model.contains(":cloud") || model.contains("-cloud")))
}

fn resolved_api_key_env(provider: Provider, api_key_env: Option<&str>) -> String {
    api_key_env
        .map(str::trim)
        .filter(|env_key| !env_key.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| api_key_env_var(provider.as_ref()))
}

pub fn heuristic_provider_from_model(model: &str) -> Option<Provider> {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains(':') && !trimmed.contains('/') && !trimmed.contains('@') {
        return Some(Provider::Ollama);
    }

    let model = trimmed.to_ascii_lowercase();
    if model.starts_with("muse-spark-") {
        Some(Provider::Meta)
    } else if model.starts_with("gpt-oss-")
        || model.starts_with("gpt-")
        || model.starts_with("o1")
        || model.starts_with("o3")
        || model.starts_with("o4")
        || model.starts_with("codex")
    {
        Some(Provider::OpenAI)
    } else if model == "copilot" || model.starts_with("copilot-") {
        Some(Provider::Copilot)
    } else if model.starts_with("claude-") {
        Some(Provider::Anthropic)
    } else if model.starts_with("deepseek-") {
        Some(Provider::DeepSeek)
    } else if model.starts_with("mistral-") || model.starts_with("ministral-") || model.starts_with("codestral-") {
        Some(Provider::Mistral)
    } else if model.contains("gemini") || model.starts_with("palm") {
        Some(Provider::Gemini)
    } else if model.starts_with("glm-") {
        Some(Provider::ZAI)
    } else if model.starts_with("lmstudio-community/") {
        Some(Provider::LmStudio)
    } else if model.starts_with("mimo-") {
        Some(Provider::MiMo)
    } else if model.starts_with("qwen3.") || model.starts_with("qwen-") {
        Some(Provider::Qwen)
    } else if model.starts_with("step-") {
        Some(Provider::StepFun)
    } else if model.starts_with("moonshot-") || model.starts_with("kimi-") {
        Some(Provider::Moonshot)
    } else if model.starts_with("opencode/") || model.starts_with("opencode-zen/") {
        Some(Provider::OpenCodeZen)
    } else if model.starts_with("opencode-go/") {
        Some(Provider::OpenCodeGo)
    } else if model.starts_with("poolside/") {
        Some(Provider::Poolside)
    } else if model.starts_with("nvidia/") {
        Some(Provider::NVIDIA)
    } else if model.starts_with("deepseek-ai/")
        || model.starts_with("openai/gpt-oss-")
        || model.starts_with("zai-org/")
        || model.starts_with("moonshotai/")
        || model.starts_with("minimaxai/")
    {
        Some(Provider::HuggingFace)
    } else if model.starts_with("mixtral-")
        || model.starts_with("qwen-")
        || model.starts_with("meta-")
        || model.starts_with("llama-")
        || model.starts_with("command-")
        || model.contains('/')
        || model.contains('@')
    {
        Some(Provider::OpenRouter)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_prefers_catalogue_match_over_heuristic() {
        let resolved = ModelResolver::resolve(None, "gpt-5.6-sol", &[], None).expect("model");

        assert_eq!(resolved.provider, Provider::OpenAI);
        assert!(resolved.known_model());
        assert_eq!(resolved.display_name(), "GPT-5.6 Sol");
    }

    #[test]
    fn resolver_disambiguates_shared_openai_opencode_go_slug() {
        let bare = ModelResolver::resolve(None, "gpt-5.6-luna", &[], None).expect("bare model");
        assert_eq!(bare.provider, Provider::OpenAI);
        assert!(bare.known_model());
        assert_eq!(bare.display_name(), "GPT-5.6 Luna");

        let go = ModelResolver::resolve(None, "opencode-go/gpt-5.6-luna", &[], None).expect("opencode go");
        assert_eq!(go.provider, Provider::OpenCodeGo);
        assert!(go.known_model());
        assert_eq!(go.display_name(), "GPT-5.6 Luna (OpenCode Go)");
    }

    #[test]
    fn resolver_routes_opencode_zen_prefix_to_provider() {
        let zen = ModelResolver::resolve(None, "opencode/gpt-5.6-luna", &[], None).expect("opencode zen");

        assert_eq!(zen.provider, Provider::OpenCodeZen);
        assert_eq!(zen.model_id, "opencode/gpt-5.6-luna");
    }

    #[test]
    fn resolver_routes_nvidia_namespace_to_nvidia_provider() {
        let resolved = ModelResolver::resolve(None, "nvidia/nemotron-3-ultra-550b-a55b", &[], None).expect("model");

        assert_eq!(resolved.provider, Provider::NVIDIA);
        assert!(resolved.known_model());
    }

    #[test]
    fn resolver_uses_explicit_merge_gateway_provider_for_arbitrary_route() {
        let resolved =
            ModelResolver::resolve(Some("merge-gateway"), "deepseek/deepseek-v4-pro", &[], None).expect("route");

        assert_eq!(resolved.provider, Provider::MergeGateway);
        assert_eq!(resolved.model_id, "deepseek/deepseek-v4-pro");
        assert!(!resolved.known_model());
    }

    #[test]
    fn resolver_advertises_reasoning_only_for_known_merge_reasoning_routes() {
        let reasoning =
            ModelResolver::resolve(Some("merge-gateway"), "openai/gpt-5.5", &[], None).expect("Merge route");
        assert!(reasoning.known_model());
        assert!(reasoning.reasoning_supported());

        let routing = ModelResolver::resolve(Some("merge-gateway"), "default_routing", &[], None).expect("Merge route");
        assert!(routing.known_model());
        assert!(!routing.reasoning_supported());
    }

    #[test]
    fn resolver_keeps_official_meta_and_openrouter_meta_models_distinct() {
        let official = ModelResolver::resolve(None, "muse-spark-1.2", &[], None).expect("official Meta model");
        assert_eq!(official.provider, Provider::Meta);
        assert!(official.known_model());

        let marketplace = ModelResolver::resolve(None, "meta/muse-spark-1.2", &[], None).expect("OpenRouter model");
        assert_eq!(marketplace.provider, Provider::OpenRouter);
        assert!(marketplace.known_model());
    }

    #[test]
    fn resolver_uses_provider_override_for_dynamic_model() {
        let dynamic_models = [DynamicModelRef {
            provider: Provider::Ollama,
            model_id: "custom-local-model",
        }];
        let resolved = ModelResolver::resolve(
            Some("ollama"),
            "custom-local-model",
            &dynamic_models,
            Some(DynamicModelMeta {
                display_name: "Custom Local Model".to_string(),
                description: Some("dynamic".to_string()),
                context_window: Some(32_000),
            }),
        )
        .expect("resolved model");

        assert_eq!(resolved.provider, Provider::Ollama);
        assert!(!resolved.known_model());
        assert_eq!(resolved.context_window(), Some(32_000));
    }

    #[test]
    fn resolver_carries_provider_api_key_override() {
        let resolved = ModelResolver::resolve_with_mode_and_api_key_env(
            Some("openai"),
            "gpt-5.6-sol",
            &[],
            None,
            Some("CORPORATE_OPENAI_KEY"),
            AuthCredentialsStoreMode::File,
        )
        .expect("model");

        assert_eq!(resolved.api_key_env, "CORPORATE_OPENAI_KEY");
        assert_eq!(resolved.env_key(), "CORPORATE_OPENAI_KEY");
    }

    #[test]
    fn estimate_cost_uses_usage_totals() {
        let pricing = ModelPricing {
            input: Some(0.001),
            output: Some(0.002),
            cache_read: Some(0.0001),
            cache_write: Some(0.0002),
        };
        let usage = Usage {
            prompt_tokens: 100,
            completion_tokens: 50,
            total_tokens: 150,
            cached_prompt_tokens: Some(20),
            cache_creation_tokens: Some(10),
            cache_read_tokens: None,
            iterations: None,
        };

        let total = ModelResolver::estimate_cost(pricing, &usage).expect("cost");
        assert!(total > 0.0);
    }
}
