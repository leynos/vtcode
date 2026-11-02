//! Tests for the Lody usage extension notification.

use super::*;
use agent_client_protocol::{Agent, Builder, Client, ConnectionTo, RunWithConnectionTo, on_receive_notification};
use proptest::prelude::*;
use tokio::sync::{Notify, mpsc};
use vtcode_config::{CustomProviderPricingConfig, CustomProviderProfileConfig};

#[path = "tests/transport.rs"]
mod transport;

/// Assert normalized usage maps to the Lody delta contract.
#[test]
fn maps_normalized_usage_to_the_lody_delta_contract() {
    let usage = Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        reasoning_output_tokens: Some(3),
        cached_prompt_tokens: Some(5),
        cache_creation_tokens: Some(3),
        ..Usage::default()
    };
    let notification = usage_notification(
        &acp::SessionId::new(Arc::from("session-1")),
        "model-a",
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
    )
    .expect("usage notification");
    let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

    assert_eq!(notification.method.as_ref(), LODY_SESSION_USAGE_UPDATE_METHOD);
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

/// Assert the backend format decides input semantics over the provider name.
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

/// Assert cost normalization follows the selected backend, not the provider name.
#[test]
fn cost_normalization_follows_the_selected_backend_not_the_provider_name() {
    // `baseten` is a custom name either way; only the backend decides
    // whether `prompt_tokens` already includes the cached tokens.
    let usage = Usage {
        prompt_tokens: 100,
        completion_tokens: 0,
        cached_prompt_tokens: Some(50),
        ..Usage::default()
    };
    let pricing = Some(ModelPricing {
        input: Some(1.0 / 1_000_000.0),
        output: Some(0.0),
        cache_read: Some(0.0),
        cache_write: Some(0.0),
    });
    let cost = |exclusive_input| {
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("session-cost")),
            "model-a",
            &usage,
            UsageProfile {
                pricing,
                exclusive_input,
                ..UsageProfile::default()
            },
        )
        .expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");
        value["usage"]["costUSD"].as_f64().expect("costUSD")
    };

    assert!((cost(false) - 50.0 / 1_000_000.0).abs() < f64::EPSILON);
    assert!((cost(true) - 100.0 / 1_000_000.0).abs() < f64::EPSILON);
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

/// Assert a custom provider supplies its context window and pricing.
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

/// Assert an exact model profile overrides the provider defaults.
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
    let response = vtcode_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        ..vtcode_core::llm::provider::LLMResponse::new("session-model", "answer")
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

    let notification =
        usage_notification(&acp::SessionId::new(Arc::from("session-cost")), "acme-large", &usage, profile)
            .expect("usage notification");
    let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

    assert_eq!(value["usage"]["contextWindow"], 200_000);
    let cost = value["usage"]["costUSD"].as_f64().expect("costUSD");
    assert!((cost - 18.0).abs() < 1e-9, "1M input at $3/M plus 1M output at $15/M, got {cost}");
}

/// Assert a built-in provider falls back to the model catalogue.
#[test]
fn built_in_provider_falls_back_to_the_model_catalogue() {
    let profile = usage_profile(&[], "anthropic", "claude-fable-5");
    let catalog = vtcode_config::models::model_catalog_entry("anthropic", "claude-fable-5")
        .expect("catalogue entry for a shipped model");

    assert_eq!(profile.context_window, Some(catalog.context_window));
    assert!(profile.exclusive_input);
}

/// Assert a response without usage produces no notification.
#[test]
fn response_without_usage_produces_no_notification() {
    let response = vtcode_core::llm::provider::LLMResponse::new("model-a", "answer");

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
    let response = vtcode_core::llm::provider::LLMResponse {
        model: "served-model".to_string(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..vtcode_core::llm::provider::LLMResponse::new("fallback-model", "answer")
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
    let response = vtcode_core::llm::provider::LLMResponse {
        model: String::new(),
        usage: Some(Usage {
            prompt_tokens: 4,
            completion_tokens: 2,
            ..Usage::default()
        }),
        ..vtcode_core::llm::provider::LLMResponse::new("fallback-model", "answer")
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
    /// Assert every normalized u32 usage count survives serialization.
    #[test]
    fn preserves_all_normalized_u32_usage_counts(
        input in any::<u32>(),
        output in any::<u32>(),
        cache_read in any::<u32>(),
        cache_creation in proptest::option::of(any::<u32>()),
        raw_reasoning in any::<u32>(),
        model in "[^\\p{C}]{1,48}",
    ) {
        let usage = Usage {
            prompt_tokens: input,
            completion_tokens: output,
            reasoning_output_tokens: Some(raw_reasoning),
            cache_read_tokens: Some(cache_read),
            cache_creation_tokens: cache_creation,
            ..Usage::default()
        };
        let notification = usage_notification(
            &acp::SessionId::new(Arc::from("property-session")),
            &model,
            &usage,
            UsageProfile::default(),
        ).expect("usage notification");
        let value: serde_json::Value = serde_json::from_str(notification.params.get()).expect("usage JSON");

        prop_assert_eq!(value["usage"]["inputTokens"].as_u64(), Some(u64::from(input)));
        prop_assert_eq!(
            value["usage"]["outputTokens"].as_u64(),
            Some(u64::from(output.saturating_sub(raw_reasoning.min(output)))),
        );
        prop_assert_eq!(
            value["usage"]["reasoningOutputTokens"].as_u64(),
            Some(u64::from(raw_reasoning.min(output))),
        );
        prop_assert_eq!(value["usage"]["cacheReadInputTokens"].as_u64(), Some(u64::from(cache_read)));
        match cache_creation {
            Some(count) => prop_assert_eq!(
                value["usage"]["cacheCreationInputTokens"].as_u64(),
                Some(u64::from(count)),
            ),
            None => prop_assert!(value["usage"].get("cacheCreationInputTokens").is_none()),
        }
        prop_assert_eq!(&value["modelUsage"][model.as_str()], &value["usage"]);
    }
}
