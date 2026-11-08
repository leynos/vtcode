//! Tests for the Lody usage extension notification.
//!
//! The domain boundary owns profile resolution and cost normalization; these
//! tests keep the adapter honest about projecting the resolved turn onto the
//! Lody wire contract, including the optional fields it must not invent.

use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use diapason_config::models::ModelPricing;
use diapason_config::{
    CustomProviderApiFormat, CustomProviderConfig, CustomProviderPricingConfig, CustomProviderProfileConfig,
};
use proptest::prelude::*;
use tokio::sync::{Notify, mpsc};

#[path = "tests/transport.rs"]
mod transport;

/// Resolve `usage` for the Lody projection with the given profile.
fn resolved_notification(usage: &Usage, profile: UsageProfile) -> serde_json::Value {
    let notification =
        usage_notification(&acp::SessionId::new(Arc::from("session-1")), "model-a", resolve_turn_usage(usage, profile))
            .expect("usage notification");
    serde_json::from_str(notification.params.get()).expect("usage JSON")
}

/// Assert resolved usage maps to the Lody delta contract.
#[test]
fn maps_resolved_usage_to_the_lody_delta_contract() {
    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        reasoning_output_tokens: Some(3),
        cached_prompt_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };
    let value = resolved_notification(
        &usage,
        UsageProfile {
            context_window: Some(1_048_576),
            pricing: Some(ModelPricing {
                input: Some(0.15 / 1_000_000.0),
                output: Some(0.50 / 1_000_000.0),
                cache_read: Some(0.03 / 1_000_000.0),
                cache_write: None,
            }),
            exclusive_input: false,
        },
    );

    assert_eq!(value["sessionId"], "session-1");
    assert_eq!(value["usage"]["inputTokens"], 21);
    assert_eq!(value["usage"]["outputTokens"], 5);
    assert_eq!(value["usage"]["reasoningOutputTokens"], 3);
    assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
    assert_eq!(value["usage"]["contextWindow"], 1_048_576);
    assert!(value["usage"]["costUSD"].as_f64().is_some_and(|cost| cost > 0.0));
    assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
}

/// Assert an exclusive prompt count reaches the wire as the documented total.
///
/// Anthropic-style backends report `prompt_tokens` without the cache-read and
/// cache-creation tokens. The delta must publish the full prompt total in
/// `inputTokens` and keep the cached counts in their own fields, so a client
/// that adds the three fields together does not double-count.
#[test]
fn exclusive_prompt_count_reaches_the_delta_as_the_total() {
    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        cache_read_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };

    let value = resolved_notification(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });

    assert_eq!(value["usage"]["inputTokens"], 29, "21 uncached + 5 cached + 3 cache-creation");
    assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);
    assert_eq!(value["modelUsage"]["model-a"], value["usage"]);
}

/// Assert the inclusive and exclusive representations of one turn agree.
///
/// Both describe the same tokens; only the provider's own `prompt_tokens`
/// convention differs. The published `inputTokens` total must therefore match.
#[test]
fn inclusive_and_exclusive_representations_publish_the_same_input_total() {
    let cached = 5;
    let created = 3;
    let uncached = 21;
    let exclusive = Usage {
        prompt_tokens: uncached,
        cache_read_tokens: Some(cached),
        cache_creation_tokens: Some(created),
        ..Usage::default()
    };
    let inclusive = Usage {
        prompt_tokens: uncached + cached + created,
        cache_read_tokens: Some(cached),
        cache_creation_tokens: Some(created),
        ..Usage::default()
    };

    let exclusive_value =
        resolved_notification(&exclusive, UsageProfile { exclusive_input: true, ..UsageProfile::default() });
    let inclusive_value = resolved_notification(&inclusive, UsageProfile::default());

    assert_eq!(exclusive_value["usage"]["inputTokens"], 29);
    assert_eq!(inclusive_value["usage"]["inputTokens"], 29, "an inclusive count already contains the cached tokens");
    assert_eq!(inclusive_value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(exclusive_value["usage"]["cacheReadInputTokens"], 5);
}

/// Assert optional fields stay absent when the profile supplies none.
#[test]
fn absent_profile_fields_are_not_projected_as_zero_or_invented() {
    let usage = Usage {
        prompt_tokens: 6,
        completion_tokens: 2,
        ..Usage::default()
    };

    let value = resolved_notification(&usage, UsageProfile::default());

    assert!(value["usage"].get("costUSD").is_none(), "no declared rates must mean no cost");
    assert!(value["usage"].get("contextWindow").is_none(), "no declared window must stay absent");
    assert!(value["usage"].get("cacheCreationInputTokens").is_none());
    assert!(value["usage"].get("reasoningOutputTokens").is_none());
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

/// Assert the usage profile follows the model the provider actually served.
#[test]
fn response_pricing_follows_the_model_the_provider_served() {
    let providers = vec![CustomProviderConfig {
        context_window: Some(200_000),
        pricing: per_million(3.0, 15.0),
        profiles: BTreeMap::from([(
            "served-model".to_string(),
            CustomProviderProfileConfig {
                context_window: Some(32_768),
                pricing: per_million(0.25, 1.0),
                ..CustomProviderProfileConfig::default()
            },
        )]),
        ..custom_provider("acme", CustomProviderApiFormat::OpenAIChat, None, CustomProviderPricingConfig::default())
    }];
    let response = diapason_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        ..diapason_core::llm::provider::LLMResponse::new("session-model", "answer")
    };

    let profile = usage_profile(&providers, "acme", effective_model("session-model", &response));

    assert_eq!(
        profile.context_window,
        Some(32_768),
        "pricing must describe the served model, not the session's requested model"
    );
}

/// Assert a custom provider profile drives the notification window and cost.
#[test]
fn custom_provider_profile_drives_the_notification_context_window_and_cost() {
    let providers = vec![custom_provider(
        "acme",
        CustomProviderApiFormat::AnthropicMessages,
        Some(200_000),
        per_million(3.0, 15.0),
    )];
    let profile = usage_profile(&providers, "acme", "acme-large");
    let usage = Usage {
        prompt_tokens: 1_000_000,
        completion_tokens: 1_000_000,
        ..Usage::default()
    };

    let value = resolved_notification(&usage, profile);

    assert_eq!(value["usage"]["contextWindow"], 200_000);
    let cost = value["usage"]["costUSD"].as_f64().expect("costUSD");
    assert!((cost - 18.0).abs() < 1e-9, "1M input at $3/M plus 1M output at $15/M, got {cost}");
}

/// Assert a response without usage produces no notification.
#[test]
fn response_without_usage_produces_no_notification() {
    let response = diapason_core::llm::provider::LLMResponse::new("model-a", "answer");

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        "fallback-model",
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification");

    assert!(notification.is_none());
}

/// Read back the model keys a usage notification attributes the usage to.
fn reported_model_names(notification: acp::ExtNotification) -> Vec<String> {
    let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");
    let model_usage = value["modelUsage"].as_object().expect("modelUsage object");
    model_usage.keys().cloned().collect()
}

/// Assert usage is attributed to the model the provider reported.
#[test]
fn response_usage_is_attributed_to_the_model_the_provider_served() {
    let response = diapason_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..diapason_core::llm::provider::LLMResponse::new("fallback-model", "answer")
    };

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        effective_model("fallback-model", &response),
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification")
    .expect("usage notification");

    assert_eq!(reported_model_names(notification), vec!["served-model".to_string()]);
}

/// Assert a response without a model uses the session model.
#[test]
fn response_without_a_model_falls_back_to_the_session_model() {
    let response = diapason_core::llm::provider::LLMResponse {
        model: String::new(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..diapason_core::llm::provider::LLMResponse::new("fallback-model", "answer")
    };

    let notification = response_usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        effective_model("fallback-model", &response),
        &response,
        UsageProfile::default(),
    )
    .expect("optional usage notification")
    .expect("usage notification");

    assert_eq!(reported_model_names(notification), vec!["fallback-model".to_string()]);
}

/// Generate a `u32` count, favouring the values where a sum can saturate.
fn boundary_or_arbitrary_u32() -> impl Strategy<Value = u32> {
    prop_oneof![Just(0u32), Just(1u32), Just(u32::MAX - 1), Just(u32::MAX), any::<u32>(),]
}

proptest! {
    /// Assert the resolved input total saturates rather than wrapping, and that
    /// both ACP input fields carry it.
    ///
    /// The optional cache counts mean both the "provider reported a cache
    /// count" and the "provider reported none" shapes are generated. The
    /// expected total is computed here from the generated counts rather than
    /// from the production resolver, so a regression that wraps, drops the
    /// cached tokens, or publishes the provider's raw count fails the property
    /// instead of agreeing with itself.
    #[test]
    fn resolved_input_total_saturates_and_reaches_both_usage_fields(
        uncached in boundary_or_arbitrary_u32(),
        cache_read in proptest::option::of(boundary_or_arbitrary_u32()),
        cache_creation in proptest::option::of(boundary_or_arbitrary_u32()),
    ) {
        let read = cache_read.unwrap_or(0);
        let creation = cache_creation.unwrap_or(0);
        // The documented total is uncached + cached + cache-creation tokens,
        // reported at the `u32` ceiling when the sum overflows.
        let expected_total = u32::try_from(
            u64::from(uncached) + u64::from(read) + u64::from(creation),
        )
        .unwrap_or(u32::MAX);
        let usage = Usage {
            prompt_tokens: uncached,
            completion_tokens: 0,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
            ..Usage::default()
        };
        let exclusive = resolved_notification(&usage, UsageProfile { exclusive_input: true, ..UsageProfile::default() });
        // An inclusive representation of the same turn carries the total in
        // its own prompt count, saturating there exactly as the resolver does.
        let inclusive = resolved_notification(
            &Usage {
                prompt_tokens: uncached.saturating_add(read).saturating_add(creation),
                ..usage
            },
            UsageProfile::default(),
        );

        for (representation, value) in [("exclusive", &exclusive), ("inclusive", &inclusive)] {
            prop_assert_eq!(
                value["usage"]["inputTokens"].as_u64(),
                Some(u64::from(expected_total)),
                "{} usage.inputTokens must report the saturated total",
                representation
            );
            prop_assert_eq!(
                value["modelUsage"]["model-a"]["inputTokens"].as_u64(),
                Some(u64::from(expected_total)),
                "{} modelUsage.inputTokens must report the saturated total",
                representation
            );
            prop_assert_eq!(
                value["usage"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(read)),
                "{} usage.cacheReadInputTokens must report the cached count",
                representation
            );
            prop_assert_eq!(
                value["modelUsage"]["model-a"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(read)),
                "{} modelUsage.cacheReadInputTokens must report the cached count",
                representation
            );
            match cache_creation {
                Some(count) => {
                    prop_assert_eq!(
                        value["usage"]["cacheCreationInputTokens"].as_u64(),
                        Some(u64::from(count)),
                        "{} usage.cacheCreationInputTokens must report the written count",
                        representation
                    );
                    prop_assert_eq!(
                        value["modelUsage"]["model-a"]["cacheCreationInputTokens"].as_u64(),
                        Some(u64::from(count)),
                        "{} modelUsage.cacheCreationInputTokens must report the written count",
                        representation
                    );
                }
                None => {
                    prop_assert!(
                        value["usage"].get("cacheCreationInputTokens").is_none(),
                        "{} must not invent a cache-creation count",
                        representation
                    );
                    prop_assert!(
                        value["modelUsage"]["model-a"].get("cacheCreationInputTokens").is_none(),
                        "{} must not invent a cache-creation count",
                        representation
                    );
                }
            }
        }
        prop_assert_eq!(&exclusive["modelUsage"]["model-a"], &exclusive["usage"]);
        prop_assert_eq!(&inclusive["modelUsage"]["model-a"], &inclusive["usage"]);
    }

    /// Assert every resolved u32 usage count survives serialization.
    #[test]
    fn preserves_all_resolved_u32_usage_counts(
        input in any::<u32>(),
        output in any::<u32>(),
        cache_read in any::<u32>(),
        cache_creation in proptest::option::of(any::<u32>()),
        reasoning in proptest::option::of(any::<u32>()),
        model in "[^\\p{C}]{1,48}",
    ) {
        let resolved = TurnUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_creation,
            reasoning_output_tokens: reasoning,
            cost_usd: None,
            context_window: None,
        };
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("property-session")),
            &model,
            resolved,
        ).expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

        prop_assert_eq!(value["usage"]["inputTokens"].as_u64(), Some(u64::from(input)));
        prop_assert_eq!(value["usage"]["outputTokens"].as_u64(), Some(u64::from(output)));
        prop_assert_eq!(value["usage"]["cacheReadInputTokens"].as_u64(), Some(u64::from(cache_read)));
        match cache_creation {
            Some(count) => prop_assert_eq!(
                value["usage"]["cacheCreationInputTokens"].as_u64(),
                Some(u64::from(count)),
            ),
            None => prop_assert!(value["usage"].get("cacheCreationInputTokens").is_none()),
        }
        match reasoning {
            Some(count) => prop_assert_eq!(
                value["usage"]["reasoningOutputTokens"].as_u64(),
                Some(u64::from(count)),
            ),
            None => prop_assert!(value["usage"].get("reasoningOutputTokens").is_none()),
        }
        prop_assert_eq!(&value["modelUsage"][model.as_str()], &value["usage"]);
    }

    /// Assert equivalent inclusive and exclusive input representations price
    /// the same turn identically on the wire, and that both match an
    /// independently computed expected cost.
    ///
    /// `prompt_tokens` either already contains the cache-read and
    /// cache-creation tokens (OpenAI-style) or excludes them
    /// (Anthropic-style). The notification must report the provider's own
    /// count but a cost that depends only on the underlying token set and the
    /// declared rates, including the optional cache rates.
    ///
    /// The expected value is derived here from the generated counts and rates
    /// rather than from the production estimator, so a regression that
    /// returns zero, ignores the rates, or double-counts reasoning tokens
    /// fails the property instead of merely agreeing with itself.
    #[test]
    fn equivalent_inclusive_and_exclusive_inputs_report_the_same_cost(
        uncached in 0u32..1_000_000,
        cache_read in 0u32..1_000_000,
        cache_creation in 0u32..1_000_000,
        output in 0u32..1_000_000,
        reasoning in proptest::option::of(0u32..1_000_000),
        input_rate in 0.0f64..100.0,
        output_rate in 0.0f64..100.0,
        cache_read_rate in proptest::option::of(0.0f64..100.0),
        cache_write_rate in proptest::option::of(0.0f64..100.0),
    ) {
        // Rates are declared per million tokens; the estimator works per token.
        let per_token = |rate: f64| rate / 1_000_000.0;
        let input_per_token = per_token(input_rate);
        let output_per_token = per_token(output_rate);
        let pricing = ModelPricing {
            input: Some(input_per_token),
            output: Some(output_per_token),
            cache_read: cache_read_rate.map(per_token),
            cache_write: cache_write_rate.map(per_token),
        };
        let profile = |exclusive_input| UsageProfile {
            pricing: Some(pricing),
            exclusive_input,
            ..UsageProfile::default()
        };
        let inclusive = Usage {
            prompt_tokens: uncached + cache_read + cache_creation,
            completion_tokens: output,
            reasoning_output_tokens: reasoning,
            cache_read_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_creation),
            ..Usage::default()
        };
        let exclusive = Usage {
            prompt_tokens: uncached,
            completion_tokens: output,
            reasoning_output_tokens: reasoning,
            cache_read_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_creation),
            ..Usage::default()
        };

        // Independent expectation: uncached input at the input rate, cache reads
// at the declared rate or the 10% fallback, cache creations at the
// declared rate or the 125% fallback, and the full completion count once
// at the output rate. Reasoning tokens are a subset of the completion
// count and are not charged again.
        let read_rate = cache_read_rate.map(per_token).unwrap_or(input_per_token * 0.10);
        let write_rate = cache_write_rate.map(per_token).unwrap_or(input_per_token * 1.25);
        let expected_cost = f64::from(uncached) * input_per_token
            + f64::from(cache_read) * read_rate
            + f64::from(cache_creation) * write_rate
            + f64::from(output) * output_per_token;

        let inclusive_value = resolved_notification(&inclusive, profile(false));
        let exclusive_value = resolved_notification(&exclusive, profile(true));
        let inclusive_cost = inclusive_value["usage"]["costUSD"].as_f64().expect("inclusive costUSD");
        let exclusive_cost = exclusive_value["usage"]["costUSD"].as_f64().expect("exclusive costUSD");

        // Both representations describe the same token set, so the ACP
        // `inputTokens` total is the full prompt count in both cases and the
        // cached count is reported beside it rather than double-counted.
        let full_prompt = uncached + cache_read + cache_creation;
        for (representation, value) in [("inclusive", &inclusive_value), ("exclusive", &exclusive_value)] {
            prop_assert_eq!(
                value["usage"]["inputTokens"].as_u64(),
                Some(u64::from(full_prompt)),
                "{} inputTokens must report the total prompt count",
                representation
            );
            prop_assert_eq!(
                value["usage"]["cacheReadInputTokens"].as_u64(),
                Some(u64::from(cache_read)),
                "{} cacheReadInputTokens must report the cached count",
                representation
            );
        }

        let tolerance = 1e-9 * expected_cost.abs().max(1.0);
        prop_assert!(
            (inclusive_cost - expected_cost).abs() <= tolerance,
            "inclusive {inclusive_cost} vs expected {expected_cost}"
        );
        prop_assert!(
            (exclusive_cost - expected_cost).abs() <= tolerance,
            "exclusive {exclusive_cost} vs expected {expected_cost}"
        );
        prop_assert!(
            (inclusive_cost - exclusive_cost).abs() <= tolerance,
            "inclusive {inclusive_cost} vs exclusive {exclusive_cost}"
        );
    }
}
