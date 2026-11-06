//! Provider-normalized usage accumulation and cache-aware session cost estimation.
//!
//! Different providers report `prompt_tokens` with different cache semantics:
//! Anthropic and Minimax report `prompt_tokens` *exclusive* of cache-read and
//! cache-creation tokens, while every other supported provider (OpenAI, Gemini,
//! etc.) reports `prompt_tokens` as a total that already includes cached
//! tokens. This module normalizes per-turn provider usage into the canonical
//! harness `Usage` shape, where `input_tokens` always means the total prompt
//! tokens (uncached + cached + cache-creation), and provides a shared cost
//! estimator used by both the interactive and headless runloops.

use diapason_config::models::ModelPricing;
use diapason_config::{CustomProviderApiFormat, CustomProviderConfig};

use crate::llm::model_resolver::ModelResolver;
use crate::llm::provider::ToolDefinition;
use crate::llm::provider::Usage as ProviderUsage;

/// Estimate the token overhead of sending `tools` in the request payload.
///
/// Serializes each tool definition to JSON (the wire format sent to
/// providers), sums the byte length, and converts to an approximate token
/// count using the same "~4 bytes per token" heuristic used elsewhere in the
/// codebase (see `system.rs` and `progress.rs`). A tool whose definition
/// fails to serialize contributes zero bytes rather than failing the whole
/// estimate, since this is an advisory figure, not a billing-accurate count.
pub fn estimate_tool_definition_tokens(tools: &[ToolDefinition]) -> u64 {
    // Reuse a single buffer across all tools instead of allocating a fresh
    // `String` per definition. The buffer grows once to the largest serialized
    // tool and stays there for the rest of the call.
    let mut buf = Vec::new();
    let total_bytes: u64 = tools
        .iter()
        .map(|tool| {
            buf.clear();
            serde_json::to_writer(&mut buf, tool).map(|_| buf.len() as u64).unwrap_or(0)
        })
        .sum();
    total_bytes.div_ceil(4)
}

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

/// Build a per-turn harness `Usage` sample from raw provider usage, applying
/// the provider-specific normalization documented on
/// [`provider_reports_exclusive_input`] so `input_tokens` always represents
/// the total prompt token count across every provider.
pub fn normalized_turn_usage(provider: &str, usage: &ProviderUsage) -> diapason_exec_events::Usage {
    normalized_turn_usage_with_input_semantics(provider_reports_exclusive_input(provider), usage)
}

/// Build a per-turn harness `Usage` sample from raw provider usage using an
/// explicit input-semantics decision.
///
/// Callers that select a wire backend rather than a provider name — a custom
/// provider routed to the Anthropic Messages API reports cache counts
/// separately even though its name is not `anthropic` — pass
/// `exclusive_input` directly instead of relying on
/// [`provider_reports_exclusive_input`]'s name match.
pub fn normalized_turn_usage_with_input_semantics(
    exclusive_input: bool,
    usage: &ProviderUsage,
) -> diapason_exec_events::Usage {
    let cached = u64::from(usage.cache_read_tokens_or_fallback());
    let creation = u64::from(usage.cache_creation_tokens_or_zero());
    let mut input = u64::from(usage.prompt_tokens);
    if exclusive_input {
        input = input.saturating_add(cached).saturating_add(creation);
    }
    let output = u64::from(usage.completion_tokens);

    diapason_exec_events::Usage {
        input_tokens: input,
        cached_input_tokens: cached,
        cache_creation_tokens: creation,
        output_tokens: output,
    }
}

/// Cache-aware and conservative session cost estimates in USD.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionCostEstimate {
    /// Every input token priced at the full input rate, with no cache
    /// discount applied. This is the conservative, deterministic figure used
    /// for budget enforcement.
    pub raw_usd: f64,
    /// Cache-aware estimate that discounts cache-read tokens and surcharges
    /// cache-creation tokens, for transparency in user-facing reporting.
    pub effective_usd: f64,
}

/// Resolve pricing for `provider`/`model` and estimate session costs from
/// accumulated harness usage. Returns `None` when the model cannot be
/// resolved or pricing metadata is unavailable.
pub fn estimate_session_costs(
    provider: &str,
    model: &str,
    usage: &diapason_exec_events::Usage,
) -> Option<SessionCostEstimate> {
    let resolved = ModelResolver::resolve(Some(provider), model, &[], None)?;
    let pricing = resolved.pricing()?;
    estimate_session_costs_with_pricing(pricing, usage)
}

/// Estimate session costs from an already-resolved [`ModelPricing`].
///
/// `effective_usd` can exceed `raw_usd` early in a session when
/// cache-creation tokens (billed at a premium) dominate the accumulated
/// usage. `raw_usd` remains the enforcement figure so budget behaviour stays
/// deterministic and discount-free.
pub fn estimate_session_costs_with_pricing(
    pricing: ModelPricing,
    usage: &diapason_exec_events::Usage,
) -> Option<SessionCostEstimate> {
    let input_rate = pricing.input?;
    let output_rate = pricing.output?;

    let input_tokens = usage.input_tokens as f64;
    let output_tokens = usage.output_tokens as f64;
    let cached_tokens = usage.cached_input_tokens as f64;
    let creation_tokens = usage.cache_creation_tokens as f64;

    let raw_usd = input_tokens * input_rate + output_tokens * output_rate;

    // Heuristic fallbacks when a model's catalog entry does not specify
    // dedicated cache rates: cache reads are assumed to cost roughly 10% of
    // the input rate, and cache writes roughly 125% of the input rate.
    let read_rate = pricing.cache_read.unwrap_or(input_rate * 0.10);
    let write_rate = pricing.cache_write.unwrap_or(input_rate * 1.25);

    let uncached_tokens = usage
        .input_tokens
        .saturating_sub(usage.cached_input_tokens)
        .saturating_sub(usage.cache_creation_tokens) as f64;

    let effective_usd = uncached_tokens * input_rate
        + cached_tokens * read_rate
        + creation_tokens * write_rate
        + output_tokens * output_rate;

    Some(SessionCostEstimate { raw_usd, effective_usd })
}

/// Reporting profile resolved for one provider/model pair.
///
/// Adapters that publish usage metadata need the same three facts for every
/// wire protocol: whether `prompt_tokens` excludes cached tokens, the context
/// window to advertise, and the rates to price the turn with. Resolving them
/// here keeps that policy independent of any one adapter's projection.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct UsageProfile {
    /// Advertised context window, absent when no profile declares one.
    pub context_window: Option<usize>,
    /// Rates used for cost reporting, absent when none are declared.
    pub pricing: Option<ModelPricing>,
    /// Whether `prompt_tokens` excludes cached prompt tokens, so cost
    /// normalization must add them back.
    pub exclusive_input: bool,
}

/// One turn's provider usage resolved into the shared reporting vocabulary.
///
/// Reasoning tokens are reported separately and subtracted from
/// `output_tokens`, so the two fields describe disjoint token sets. Every
/// optional field is present only when the provider or profile supplied it:
/// none of them is invented to make a payload look complete.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TurnUsage {
    /// Total prompt tokens (uncached + cached + cache-creation).
    pub input_tokens: u32,
    /// Completion tokens excluding reasoning tokens.
    pub output_tokens: u32,
    /// Tokens served from the provider's prompt cache.
    pub cache_read_input_tokens: u32,
    /// Tokens written to the provider's prompt cache, when reported.
    pub cache_creation_input_tokens: Option<u32>,
    /// Reasoning tokens included in the provider's completion count, when
    /// reported.
    pub reasoning_output_tokens: Option<u32>,
    /// Cache-aware cost of the turn in USD, when both rates are known.
    pub cost_usd: Option<f64>,
    /// Context window to advertise, when the resolved profile declares one.
    pub context_window: Option<usize>,
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

/// Resolve one turn's provider usage against its reporting profile.
///
/// Cost is computed in the provider's own input-token semantics rather than
/// reinterpreting a shared total, so an inclusive and an exclusive
/// representation of the same tokens price identically.
#[must_use]
pub fn resolve_turn_usage(usage: &ProviderUsage, profile: UsageProfile) -> TurnUsage {
    let reasoning_output_tokens = usage
        .reasoning_output_tokens
        .map(|reasoning| reasoning.min(usage.completion_tokens));
    let output_tokens = usage
        .completion_tokens
        .saturating_sub(reasoning_output_tokens.unwrap_or_default());
    let cost_usd = profile.pricing.and_then(|pricing| {
        let normalized = normalized_turn_usage_with_input_semantics(profile.exclusive_input, usage);
        estimate_session_costs_with_pricing(pricing, &normalized).map(|estimate| estimate.effective_usd)
    });

    TurnUsage {
        input_tokens: usage.prompt_tokens,
        output_tokens,
        cache_read_input_tokens: usage.cache_read_tokens_or_fallback(),
        cache_creation_input_tokens: usage.cache_creation_tokens,
        reasoning_output_tokens,
        cost_usd,
        context_window: profile.context_window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use serde_json::json;
    use diapason_config::{CustomProviderPricingConfig, CustomProviderProfileConfig};

    fn approx_eq(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-12, "expected {a} to approx-equal {b}");
    }

    #[test]
    fn estimate_tool_definition_tokens_is_zero_for_empty_slice() {
        assert_eq!(estimate_tool_definition_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tool_definition_tokens_matches_serialized_byte_length() {
        let tool = ToolDefinition::function(
            "read_file".to_string(),
            "Read the contents of a file from the workspace.".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"],
            }),
        );

        let expected_bytes = serde_json::to_string(&tool).expect("tool serializes").len() as u64;
        let expected_tokens = expected_bytes.div_ceil(4);

        assert_eq!(estimate_tool_definition_tokens(&[tool]), expected_tokens);
    }

    #[test]
    fn normalized_turn_usage_adds_cache_tokens_for_anthropic() {
        let usage = ProviderUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            reasoning_output_tokens: None,
            total_tokens: 120,
            cached_prompt_tokens: None,
            cache_creation_tokens: Some(50),
            cache_read_tokens: Some(400),
            iterations: None,
        };

        let normalized = normalized_turn_usage("anthropic", &usage);
        assert_eq!(normalized.input_tokens, 550);
        assert_eq!(normalized.cached_input_tokens, 400);
        assert_eq!(normalized.cache_creation_tokens, 50);
        assert_eq!(normalized.output_tokens, 20);
    }

    #[test]
    fn normalized_turn_usage_treats_minimax_like_anthropic() {
        let usage = ProviderUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            reasoning_output_tokens: None,
            total_tokens: 120,
            cached_prompt_tokens: None,
            cache_creation_tokens: Some(50),
            cache_read_tokens: Some(400),
            iterations: None,
        };

        let normalized = normalized_turn_usage("minimax", &usage);
        assert_eq!(normalized.input_tokens, 550);
        assert_eq!(normalized.cached_input_tokens, 400);
        assert_eq!(normalized.cache_creation_tokens, 50);
        assert_eq!(normalized.output_tokens, 20);
    }

    #[test]
    fn explicit_input_semantics_override_the_provider_name() {
        let usage = ProviderUsage {
            prompt_tokens: 100,
            completion_tokens: 20,
            reasoning_output_tokens: None,
            total_tokens: 120,
            cached_prompt_tokens: None,
            cache_creation_tokens: Some(50),
            cache_read_tokens: Some(400),
            iterations: None,
        };

        let exclusive = normalized_turn_usage_with_input_semantics(true, &usage);
        assert_eq!(exclusive.input_tokens, 550);
        let inclusive = normalized_turn_usage_with_input_semantics(false, &usage);
        assert_eq!(inclusive.input_tokens, 100);
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
    fn cost_normalization_follows_the_selected_backend_not_the_provider_name() {
        // `baseten` is a custom name either way; only the backend decides
        // whether `prompt_tokens` already includes the cached tokens.
        let usage = ProviderUsage {
            prompt_tokens: 100,
            completion_tokens: 0,
            cached_prompt_tokens: Some(50),
            ..ProviderUsage::default()
        };
        let pricing = Some(ModelPricing {
            input: Some(1.0 / 1_000_000.0),
            output: Some(0.0),
            cache_read: Some(0.0),
            cache_write: Some(0.0),
        });
        let cost = |exclusive_input| {
            resolve_turn_usage(
                &usage,
                UsageProfile {
                    pricing,
                    exclusive_input,
                    ..UsageProfile::default()
                },
            )
            .cost_usd
            .expect("costUSD")
        };

        assert!((cost(false) - 50.0 / 1_000_000.0).abs() < f64::EPSILON);
        assert!((cost(true) - 100.0 / 1_000_000.0).abs() < f64::EPSILON);
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

    #[test]
    fn resolved_turn_usage_reports_reasoning_separately_from_output() {
        let usage = ProviderUsage {
            prompt_tokens: 21,
            completion_tokens: 8,
            reasoning_output_tokens: Some(3),
            cached_prompt_tokens: Some(5),
            cache_creation_tokens: Some(3),
            ..ProviderUsage::default()
        };

        let resolved = resolve_turn_usage(&usage, UsageProfile::default());

        assert_eq!(resolved.input_tokens, 21);
        assert_eq!(resolved.output_tokens, 5);
        assert_eq!(resolved.reasoning_output_tokens, Some(3));
        assert_eq!(resolved.cache_read_input_tokens, 5);
        assert_eq!(resolved.cache_creation_input_tokens, Some(3));
        assert_eq!(resolved.cost_usd, None, "no pricing must not invent a cost");
        assert_eq!(resolved.context_window, None);
    }

    #[test]
    fn resolved_turn_usage_clamps_reasoning_to_the_completion_count() {
        let usage = ProviderUsage {
            prompt_tokens: 4,
            completion_tokens: 2,
            reasoning_output_tokens: Some(9),
            ..ProviderUsage::default()
        };

        let resolved = resolve_turn_usage(&usage, UsageProfile::default());

        assert_eq!(resolved.reasoning_output_tokens, Some(2));
        assert_eq!(resolved.output_tokens, 0, "reasoning must never exceed the completion count");
    }

    #[test]
    fn normalized_turn_usage_keeps_openai_prompt_tokens_as_total() {
        let usage = ProviderUsage {
            prompt_tokens: 500,
            completion_tokens: 30,
            reasoning_output_tokens: None,
            total_tokens: 530,
            cached_prompt_tokens: Some(400),
            cache_creation_tokens: None,
            cache_read_tokens: None,
            iterations: None,
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
    fn estimate_session_costs_with_pricing_discounts_cache_reads() {
        let pricing = ModelPricing {
            input: Some(0.01),
            output: Some(0.02),
            cache_read: Some(0.001),
            cache_write: Some(0.0125),
        };
        let usage = diapason_exec_events::Usage {
            input_tokens: 1_000,
            cached_input_tokens: 800,
            cache_creation_tokens: 0,
            output_tokens: 100,
        };

        let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");

        // raw: all 1000 input tokens at full rate + output.
        approx_eq(estimate.raw_usd, 1_000.0 * 0.01 + 100.0 * 0.02);
        // effective: 200 uncached @ input rate + 800 cached @ read rate + output.
        approx_eq(estimate.effective_usd, 200.0 * 0.01 + 800.0 * 0.001 + 100.0 * 0.02);
        assert!(estimate.effective_usd < estimate.raw_usd);
    }

    #[test]
    fn estimate_session_costs_with_pricing_matches_raw_when_no_cache_activity() {
        let pricing = ModelPricing {
            input: Some(0.01),
            output: Some(0.02),
            cache_read: Some(0.001),
            cache_write: Some(0.0125),
        };
        let usage = diapason_exec_events::Usage {
            input_tokens: 1_000,
            cached_input_tokens: 0,
            cache_creation_tokens: 0,
            output_tokens: 100,
        };

        let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");
        approx_eq(estimate.raw_usd, estimate.effective_usd);
    }

    #[test]
    fn estimate_session_costs_with_pricing_uses_heuristic_fallback_rates() {
        let pricing = ModelPricing {
            input: Some(0.01),
            output: Some(0.02),
            cache_read: None,
            cache_write: None,
        };
        let usage = diapason_exec_events::Usage {
            input_tokens: 1_000,
            cached_input_tokens: 50,
            cache_creation_tokens: 500,
            output_tokens: 50,
        };

        let estimate = estimate_session_costs_with_pricing(pricing, &usage).expect("estimate");

        let read_rate = 0.01 * 0.10;
        let write_rate = 0.01 * 1.25;
        let uncached = 1_000.0 - 50.0 - 500.0;
        let expected_effective = uncached * 0.01 + 50.0 * read_rate + 500.0 * write_rate + 50.0 * 0.02;
        approx_eq(estimate.effective_usd, expected_effective);
        approx_eq(estimate.raw_usd, 1_000.0 * 0.01 + 50.0 * 0.02);
        // Cache-creation tokens dominate here (500 vs. 50 cache-read tokens),
        // so the write-rate premium outweighs the read-rate discount and
        // pushes effective above raw.
        assert!(estimate.effective_usd > estimate.raw_usd);
    }

    #[test]
    fn estimate_session_costs_with_pricing_returns_none_without_full_pricing() {
        let missing_input = ModelPricing {
            input: None,
            output: Some(0.02),
            cache_read: None,
            cache_write: None,
        };
        let missing_output = ModelPricing {
            input: Some(0.01),
            output: None,
            cache_read: None,
            cache_write: None,
        };
        let usage = diapason_exec_events::Usage::default();

        assert!(estimate_session_costs_with_pricing(missing_input, &usage).is_none());
        assert!(estimate_session_costs_with_pricing(missing_output, &usage).is_none());
    }

    #[test]
    fn session_budget_tracks_spend_and_thresholds() {
        let mut budget = SessionBudget::new(Some(1.0));
        assert_eq!(budget.status(), BudgetStatus::Ok);
        // 0.5 -> Ok
        assert_eq!(budget.record(0.5), BudgetStatus::Ok);
        // 0.3 -> 0.8 >= 0.75 cap -> Warning
        assert_eq!(budget.record(0.3), BudgetStatus::Warning { spent: 0.8, max: 1.0 });
        // 0.3 -> 1.1 >= cap -> Exceeded
        assert_eq!(budget.record(0.3), BudgetStatus::Exceeded { spent: 1.1, max: 1.0 });
        assert!((budget.spent_usd() - 1.1).abs() < 1e-9);
        assert!((budget.remaining_usd().unwrap() - 0.0).abs() < 1e-9);
    }

    #[test]
    fn session_budget_unlimited_is_always_ok() {
        let mut budget = SessionBudget::new(None);
        assert_eq!(budget.record(1000.0), BudgetStatus::Ok);
        assert_eq!(budget.remaining_usd(), None);
    }

    #[test]
    fn budget_status_classify_matches_harness_semantics() {
        // Unlimited.
        assert_eq!(BudgetStatus::classify(999.0, None, 0.75), BudgetStatus::Ok);
        // Under warning.
        assert_eq!(BudgetStatus::classify(0.5, Some(1.0), 0.75), BudgetStatus::Ok);
        // At/above warning, within cap.
        assert_eq!(BudgetStatus::classify(0.8, Some(1.0), 0.75), BudgetStatus::Warning { spent: 0.8, max: 1.0 });
        // Exactly at cap is NOT exceeded (strict `>`), matching runner semantics.
        assert!(!BudgetStatus::classify(1.0, Some(1.0), 0.75).is_exceeded());
        // Over cap.
        assert!(BudgetStatus::classify(1.01, Some(1.0), 0.75).is_exceeded());
        // Configurable threshold.
        assert_eq!(BudgetStatus::classify(0.6, Some(1.0), 0.5), BudgetStatus::Warning { spent: 0.6, max: 1.0 });
    }
}

/// Default fraction of the budget at which the harness warns before hard
/// exhaustion. Mirrors `agent.harness.budget_warning_threshold`'s default so a
/// [`SessionBudget`] built without an explicit threshold behaves like the
/// harness default.
pub const DEFAULT_BUDGET_WARNING_RATIO: f64 = 0.75;

/// Outcome of classifying cumulative spend against a budget cap.
///
/// This is the single source of truth for the harness budget decision. Both the
/// `diapason-core` runner ([`crate::core::agent::runner`]) and the binary crate's
/// turn loop classify spend through [`BudgetStatus::classify`] rather than
/// re-deriving the `> max` / `>= threshold * max` comparisons inline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BudgetStatus {
    /// Under the warning threshold.
    Ok,
    /// At or above the warning ratio but still within the cap.
    Warning {
        /// Cumulative spend so far (USD).
        spent: f64,
        /// Configured cap (USD).
        max: f64,
    },
    /// Over the cap — the run should stop or escalate.
    Exceeded {
        /// Cumulative spend so far (USD).
        spent: f64,
        /// Configured cap (USD).
        max: f64,
    },
}

impl BudgetStatus {
    /// Classify `spent_usd` against an optional `max_usd` cap and a
    /// `warning_threshold` fraction (`0.0..=1.0`).
    ///
    /// - `max_usd == None` → always [`BudgetStatus::Ok`] (unlimited).
    /// - `spent_usd > max` → [`BudgetStatus::Exceeded`] (strict, matching the
    ///   harness "stop after reaching the budget limit" semantics).
    /// - `spent_usd >= warning_threshold * max` → [`BudgetStatus::Warning`].
    /// - otherwise → [`BudgetStatus::Ok`].
    #[must_use]
    pub fn classify(spent_usd: f64, max_usd: Option<f64>, warning_threshold: f64) -> Self {
        let Some(max) = max_usd else {
            return BudgetStatus::Ok;
        };
        if spent_usd > max {
            BudgetStatus::Exceeded { spent: spent_usd, max }
        } else if spent_usd >= warning_threshold * max {
            BudgetStatus::Warning { spent: spent_usd, max }
        } else {
            BudgetStatus::Ok
        }
    }

    /// Whether the cap has been exceeded (the run should stop/escalate).
    #[must_use]
    pub fn is_exceeded(&self) -> bool {
        matches!(self, BudgetStatus::Exceeded { .. })
    }
}

/// Durable per-session cost budget for long-running (full-auto) sessions.
///
/// Long-horizon tasks accrue cost continuously; the harness should pause or
/// escalate at thresholds rather than burning unbounded spend. `SessionBudget`
/// accumulates the conservative `raw_usd` figure (see [`SessionCostEstimate`])
/// and reports a [`BudgetStatus`] on each recorded turn. It delegates the
/// decision to [`BudgetStatus::classify`] so callers that instead recompute the
/// running total each turn (like the harness) share identical semantics.
#[derive(Debug, Clone)]
pub struct SessionBudget {
    max_usd: Option<f64>,
    warning_threshold: f64,
    spent_usd: f64,
}

impl SessionBudget {
    /// Create a budget with the default warning ratio. `None` max means
    /// unlimited (status is always `Ok`).
    #[must_use]
    pub fn new(max_usd: Option<f64>) -> Self {
        Self::with_warning_threshold(max_usd, DEFAULT_BUDGET_WARNING_RATIO)
    }

    /// Create a budget with an explicit warning threshold (e.g. from
    /// `agent.harness.budget_warning_threshold`).
    #[must_use]
    pub fn with_warning_threshold(max_usd: Option<f64>, warning_threshold: f64) -> Self {
        Self { max_usd, warning_threshold, spent_usd: 0.0 }
    }

    /// Record a turn's spend and return the resulting status.
    pub fn record(&mut self, raw_usd: f64) -> BudgetStatus {
        self.spent_usd += raw_usd.max(0.0);
        self.status()
    }

    /// Current status given accumulated spend.
    #[must_use]
    pub fn status(&self) -> BudgetStatus {
        BudgetStatus::classify(self.spent_usd, self.max_usd, self.warning_threshold)
    }

    /// Cumulative spend so far (USD).
    #[must_use]
    pub fn spent_usd(&self) -> f64 {
        self.spent_usd
    }

    /// Remaining budget, or `None` when unlimited.
    #[must_use]
    pub fn remaining_usd(&self) -> Option<f64> {
        self.max_usd.map(|m| (m - self.spent_usd).max(0.0))
    }
}
// weave: run 'weave explain crates/codegen/diapason-core/src/llm/usage_cost.rs' for per-hunk detail, 'weave check' to verify your resolution
