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

#[path = "tests/properties.rs"]
mod properties;

#[path = "tests/diagnostics.rs"]
mod diagnostics;

/// Resolve `usage` for the Lody projection with the given profile.
fn resolved_notification(usage: &Usage, profile: UsageProfile) -> serde_json::Value {
    let notification =
        usage_notification(&acp::SessionId::new(Arc::from("session-1")), "model-a", resolve_turn_usage(usage, profile))
            .expect("usage notification");
    serde_json::from_str(notification.params.get()).expect("usage JSON")
}

/// Assert resolved usage maps to the Lody delta contract.
///
/// `inputTokens` is the full prompt total, and the cache-read and
/// cache-creation fields are its breakdown: both describe tokens the provider
/// already counted inside the prompt, so a client must read `inputTokens` as
/// the total rather than adding the three fields together.
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
/// `inputTokens` — 21 uncached plus 5 cache-read plus 3 cache-creation — and
/// report the cached counts in their own fields as a breakdown of that total.
/// The cache fields are subsets, not additional counts: adding all three
/// fields would describe 37 tokens when the prompt held 29.
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

/// Assert a built-in provider's runtime label cannot stand in for its key.
///
/// `ProviderRequestRuntime::for_provider` serves every built-in provider from
/// the shared fallback runtime, whose telemetry label is `default`, so
/// resolving the profile from that label would take the inclusive branch and
/// publish 21 prompt tokens for a turn that billed 29. This exercises the
/// runtime the prompt path actually holds rather than passing the provider key
/// to the profile helper directly, which is what made the distinction
/// invisible.
#[test]
fn built_in_fallback_runtime_label_does_not_decide_input_semantics() {
    let registry = crate::zed::provider_runtime::ProviderRuntimeRegistry::new(
        &[],
        &diapason_config::TimeoutsConfig::default(),
        Arc::new(diapason_core::metrics::MetricsCollector::new()),
    );
    let runtime = registry.for_provider("anthropic");
    assert_eq!(
        runtime.provider_name(),
        "default",
        "the fallback runtime's label is shared by every built-in provider"
    );

    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 4,
        cache_read_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };
    let from_label = publication_profile(&[], runtime.provider_name(), "claude-fable-5", "claude-fable-5");
    let from_key = publication_profile(&[], "anthropic", "claude-fable-5", "claude-fable-5");

    assert!(!from_label.exclusive_input, "the `default` label takes the inclusive branch");
    assert!(from_key.exclusive_input, "the selected Anthropic key keeps exclusive semantics");
    assert_eq!(
        resolved_notification(&usage, from_label)["usage"]["inputTokens"],
        21,
        "the runtime label under-reports the prompt total"
    );
    assert_eq!(
        resolved_notification(&usage, from_key)["usage"]["inputTokens"],
        29,
        "the selected key publishes the documented total"
    );
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
    let usage = Usage {
        prompt_tokens: 1_000_000,
        completion_tokens: 1_000_000,
        ..Usage::default()
    };

    let value = resolved_notification(&usage, profile);

    assert_eq!(
        value["usage"]["contextWindow"], 32_768,
        "the window must describe the served model, not the provider's default"
    );
    let cost = value["usage"]["costUSD"].as_f64().expect("costUSD");
    assert!(
        (cost - 1.25).abs() < 1e-9,
        "1M input at $0.25/M plus 1M output at $1/M must price at $1.25, not the provider's $3/M and $15/M: got {cost}"
    );
}

/// Assert a served alias keeps the routed backend's input semantics.
///
/// A gateway can answer with a dated snapshot or alias that is not a
/// configured profile key, so looking the semantics up by the served name would
/// fall back to the provider-wide format. The request was routed through the
/// Anthropic Messages profile, whose counts exclude cache tokens from
/// `prompt_tokens`, so the published total must still include them while the
/// served name keeps its own pricing and window.
#[test]
fn served_alias_keeps_the_routed_profile_input_semantics() {
    let providers = vec![CustomProviderConfig {
        profiles: BTreeMap::from([(
            "claude-routed".to_string(),
            CustomProviderProfileConfig {
                api_format: CustomProviderApiFormat::AnthropicMessages,
                context_window: Some(200_000),
                pricing: per_million(3.0, 15.0),
                ..CustomProviderProfileConfig::default()
            },
        )]),
        ..custom_provider("acme", CustomProviderApiFormat::OpenAIChat, None, CustomProviderPricingConfig::default())
    }];

    let profile = publication_profile(&providers, "acme", "claude-routed", "claude-routed-2026-01-01");
    assert!(profile.exclusive_input, "the routed Anthropic profile decides the input semantics");

    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 4,
        cache_read_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };
    let value = resolved_notification(&usage, profile);

    assert_eq!(
        value["usage"]["inputTokens"], 29,
        "published input must carry cache read and creation tokens: {value}"
    );
    assert_eq!(value["usage"]["cacheReadInputTokens"], 5);
    assert_eq!(value["usage"]["cacheCreationInputTokens"], 3);

    let served = publication_profile(&providers, "acme", "claude-routed", "claude-routed");
    assert_eq!(served.context_window, Some(200_000), "served profile keeps its window");
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
