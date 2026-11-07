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

proptest! {
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

        let inclusive_cost = resolved_notification(&inclusive, profile(false))["usage"]["costUSD"]
            .as_f64()
            .expect("inclusive costUSD");
        let exclusive_cost = resolved_notification(&exclusive, profile(true))["usage"]["costUSD"]
            .as_f64()
            .expect("exclusive costUSD");

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
