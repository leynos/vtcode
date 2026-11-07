//! Provider/configuration adapter for usage reporting.
//!
//! The shared accounting in [`crate::llm::usage_cost`] works on a
//! domain-shaped [`UsageProfile`]: it prices and normalizes a turn once the
//! input-token semantics, context window, and rates are supplied, and it never
//! inspects provider configuration or a vendor wire format. This module is the
//! adapter that translates a provider's configuration and backend into that
//! profile, so the domain boundary stays free of vendor-specific knowledge.
//!
//! Adapters that publish usage — the ACP bridge among them — call
//! [`usage_profile`] once per turn and then project the resolved
//! [`TurnUsage`](crate::llm::usage_cost::TurnUsage) into their own wire format.

use diapason_config::models::ModelPricing;
use diapason_config::{CustomProviderApiFormat, CustomProviderConfig};

use crate::llm::model_resolver::ModelResolver;
use crate::llm::provider::Usage as ProviderUsage;
use crate::llm::usage_cost::{UsageProfile, normalized_turn_usage_with_input_semantics};

/// Returns true when `provider` reports `prompt_tokens` exclusive of
/// cache-read and cache-creation tokens.
///
/// Anthropic and Minimax (which wraps the Anthropic provider) report
/// `prompt_tokens` as the count of tokens billed at the full input rate,
/// separate from cache-read and cache-creation tokens. All other providers
/// report `prompt_tokens` as a total that already includes cached tokens, so
/// no adjustment is needed for them.
pub fn provider_reports_exclusive_input(provider: &str) -> bool {
    matches!(provider.trim().to_ascii_lowercase().as_str(), "anthropic" | "minimax")
}

/// Select the input-token semantics of the backend a request was routed to.
///
/// A custom provider's name says nothing about its wire format: one named
/// `baseten` may speak Anthropic Messages, where `input_tokens` excludes cache
/// reads, while the same name may speak an OpenAI format where it includes
/// them. `api_format` is the resolved backend, so it wins over the name.
#[must_use]
pub fn exclusive_input_semantics(api_format: Option<CustomProviderApiFormat>, provider: &str) -> bool {
    match api_format {
        Some(CustomProviderApiFormat::AnthropicMessages) => true,
        Some(CustomProviderApiFormat::OpenAIChat | CustomProviderApiFormat::OpenAIResponses) => false,
        Some(CustomProviderApiFormat::Auto) | None => provider_reports_exclusive_input(provider),
    }
}

/// Build a per-turn harness `Usage` sample from raw provider usage, applying
/// the provider-specific normalization documented on
/// [`provider_reports_exclusive_input`] so `input_tokens` always represents
/// the total prompt token count across every provider.
pub fn normalized_turn_usage(provider: &str, usage: &ProviderUsage) -> diapason_exec_events::Usage {
    normalized_turn_usage_with_input_semantics(provider_reports_exclusive_input(provider), usage)
}

/// Resolve the usage profile of the backend that served `provider`.
///
/// A configured custom provider wins over the built-in catalogue: its resolved
/// profile carries the context window and pricing the operator declared, and
/// its `api_format` names the backend that produced the usage. Otherwise the
/// built-in model catalogue supplies what it knows, and the provider name alone
/// decides the input-token semantics.
///
/// Both branches are pure functions of already-loaded configuration. The
/// catalogue lookup deliberately does not resolve credentials: reading
/// environment variables, the OAuth store, or secure storage here would make a
/// reported cost depend on whether a key happened to be installed, and would
/// run that I/O once per turn for metadata it never uses.
#[must_use]
pub fn usage_profile(custom_providers: &[CustomProviderConfig], provider: &str, model: &str) -> UsageProfile {
    if let Some(custom_provider) = custom_providers
        .iter()
        .find(|candidate| candidate.name.eq_ignore_ascii_case(provider))
    {
        let resolved = custom_provider.resolved_profile(model);
        return UsageProfile {
            context_window: resolved.context_window.filter(|window| *window > 0),
            pricing: resolved.pricing.model_pricing(),
            exclusive_input: exclusive_input_semantics(resolved.api_format, provider),
        };
    }

    ModelResolver::catalog_entry(provider, model).map_or_else(
        || UsageProfile {
            exclusive_input: exclusive_input_semantics(None, provider),
            ..UsageProfile::default()
        },
        |entry| UsageProfile {
            context_window: Some(entry.context_window).filter(|window| *window > 0),
            pricing: declared_pricing(entry.pricing),
            exclusive_input: exclusive_input_semantics(None, provider),
        },
    )
}

/// Return the pricing a catalogue entry actually declares.
///
/// An entry that states no price reports none, so usage attribution never
/// invents a cost for a model the catalogue does not price.
fn declared_pricing(pricing: ModelPricing) -> Option<ModelPricing> {
    let declared = pricing.input.is_some()
        || pricing.output.is_some()
        || pricing.cache_read.is_some()
        || pricing.cache_write.is_some();
    declared.then_some(pricing)
}

#[cfg(test)]
mod tests {
    //! Covers the provider/configuration translation into a usage profile.

    use super::*;
    use std::collections::BTreeMap;

    use diapason_config::{CustomProviderPricingConfig, CustomProviderProfileConfig};

    use crate::llm::provider::Usage;

    fn usage_with_cache_counts() -> Usage {
        Usage {
            prompt_tokens: 100,
            completion_tokens: 20,
            reasoning_output_tokens: None,
            total_tokens: 120,
            cached_prompt_tokens: None,
            cache_creation_tokens: Some(50),
            cache_read_tokens: Some(400),
            iterations: None,
        }
    }

    /// Build a custom provider with the fields usage reporting reads.
    fn custom_provider(
        name: &str,
        api_format: CustomProviderApiFormat,
        context_window: Option<usize>,
        pricing: CustomProviderPricingConfig,
    ) -> CustomProviderConfig {
        CustomProviderConfig {
            name: name.to_string(),
            display_name: name.to_string(),
            base_url: "https://llm.example/v1".to_string(),
            api_format,
            context_window,
            pricing,
            ..CustomProviderConfig::default()
        }
    }

    /// Declare pricing in the per-million-token units configuration uses.
    fn per_million(input: f64, output: f64) -> CustomProviderPricingConfig {
        CustomProviderPricingConfig {
            input_per_million_usd: Some(input),
            output_per_million_usd: Some(output),
            cache_read_per_million_usd: None,
            cache_write_per_million_usd: None,
        }
    }

    #[test]
    fn normalized_turn_usage_adds_cache_tokens_for_anthropic() {
        let normalized = normalized_turn_usage("anthropic", &usage_with_cache_counts());
        assert_eq!(normalized.input_tokens, 550);
        assert_eq!(normalized.cached_input_tokens, 400);
        assert_eq!(normalized.cache_creation_tokens, 50);
        assert_eq!(normalized.output_tokens, 20);
    }

    #[test]
    fn normalized_turn_usage_treats_minimax_like_anthropic() {
        let normalized = normalized_turn_usage("minimax", &usage_with_cache_counts());
        assert_eq!(normalized.input_tokens, 550);
        assert_eq!(normalized.cached_input_tokens, 400);
        assert_eq!(normalized.cache_creation_tokens, 50);
        assert_eq!(normalized.output_tokens, 20);
    }

    #[test]
    fn normalized_turn_usage_keeps_openai_prompt_tokens_as_total() {
        let usage = Usage {
            prompt_tokens: 500,
            completion_tokens: 30,
            cached_prompt_tokens: Some(400),
            ..Usage::default()
        };

        let normalized = normalized_turn_usage("openai", &usage);
        assert_eq!(normalized.input_tokens, 500);
        assert_eq!(normalized.cached_input_tokens, 400);
        assert_eq!(normalized.cache_creation_tokens, 0);
    }

    #[test]
    fn provider_reports_exclusive_input_is_case_insensitive() {
        assert!(provider_reports_exclusive_input("Anthropic"));
        assert!(provider_reports_exclusive_input("ANTHROPIC"));
        assert!(!provider_reports_exclusive_input("OpenAI"));
        assert!(!provider_reports_exclusive_input("openai"));
    }

    #[test]
    fn backend_selects_input_semantics_over_the_provider_name() {
        let cases = [
            (Some(CustomProviderApiFormat::AnthropicMessages), "baseten", true),
            (Some(CustomProviderApiFormat::OpenAIChat), "anthropic", false),
            (Some(CustomProviderApiFormat::OpenAIResponses), "anthropic", false),
            (Some(CustomProviderApiFormat::Auto), "anthropic", true),
            (Some(CustomProviderApiFormat::Auto), "baseten", false),
            (None, "anthropic", true),
        ];

        for (api_format, provider, expected) in cases {
            assert_eq!(
                exclusive_input_semantics(api_format, provider),
                expected,
                "api_format {api_format:?} for provider {provider}"
            );
        }
    }

    #[test]
    fn custom_provider_supplies_the_context_window_and_cost_pricing() {
        let providers = vec![custom_provider(
            "acme",
            CustomProviderApiFormat::AnthropicMessages,
            Some(200_000),
            per_million(3.0, 15.0),
        )];

        // Provider names are matched case-insensitively, as routing does.
        let profile = usage_profile(&providers, "AcMe", "acme-large");

        assert_eq!(profile.context_window, Some(200_000));
        assert!(profile.exclusive_input, "Anthropic Messages excludes cached tokens from prompt_tokens");
        let pricing = profile.pricing.expect("custom provider pricing should reach cost reporting");
        assert!((pricing.input.expect("input rate") - 3.0 / 1_000_000.0).abs() < f64::EPSILON);
        assert!((pricing.output.expect("output rate") - 15.0 / 1_000_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn model_profile_overrides_the_provider_default() {
        let providers = vec![CustomProviderConfig {
            context_window: Some(200_000),
            pricing: per_million(3.0, 15.0),
            profiles: BTreeMap::from([(
                "acme-small".to_string(),
                CustomProviderProfileConfig {
                    api_format: CustomProviderApiFormat::OpenAIChat,
                    context_window: Some(32_768),
                    pricing: per_million(0.25, 1.0),
                    ..CustomProviderProfileConfig::default()
                },
            )]),
            ..custom_provider(
                "acme",
                CustomProviderApiFormat::AnthropicMessages,
                None,
                CustomProviderPricingConfig::default(),
            )
        }];

        let profile = usage_profile(&providers, "acme", "acme-small");

        assert_eq!(profile.context_window, Some(32_768));
        assert!(
            !profile.exclusive_input,
            "the model profile's OpenAI Chat backend includes cached tokens in prompt_tokens"
        );
        let pricing = profile.pricing.expect("model profile pricing should reach cost reporting");
        assert!((pricing.input.expect("input rate") - 0.25 / 1_000_000.0).abs() < f64::EPSILON);
        assert!((pricing.output.expect("output rate") - 1.0 / 1_000_000.0).abs() < f64::EPSILON);
    }

    #[test]
    fn built_in_provider_falls_back_to_the_model_catalogue() {
        let profile = usage_profile(&[], "anthropic", "claude-fable-5");
        let catalog = diapason_config::models::model_catalog_entry("anthropic", "claude-fable-5")
            .expect("catalogue entry for a shipped model");

        assert_eq!(profile.context_window, Some(catalog.context_window));
        assert!(profile.exclusive_input);
    }
}
