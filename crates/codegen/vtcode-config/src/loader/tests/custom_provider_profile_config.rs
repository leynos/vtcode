//! Tests for custom provider profile parsing and round trips.

use super::*;

fn custom_provider_round_trip_config() -> CustomProviderConfig {
    CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        context_window: Some(256_000),
        temperature: None,
        top_p: None,
        top_k: None,
        presence_penalty: None,
        frequency_penalty: None,
        max_tokens: None,
        reasoning_effort: None,
        supports_tools: Some(true),
        supports_reasoning: Some(false),
        supports_reasoning_effort: Some(true),
        supports_vision: Some(false),
        supports_structured_output: Some(true),
        supports_parallel_tool_calls: Some(false),
        supports_context_caching: Some(true),
        supports_responses_compaction: Some(false),
        supports_stream_usage: Some(true),
        supports_context_edits: Some(true),
        api_key_env: "MYCORP_API_KEY".to_string(),
        auth: None,
        model: "gpt-5-mini".to_string(),
        models: vec!["gpt-5-mini".to_string(), "gpt-5-large".to_string()],
        profiles: std::collections::BTreeMap::from([(
            "gpt-5-mini".to_string(),
            CustomProviderProfileConfig {
                api_format: CustomProviderApiFormat::OpenAIResponses,
                context_window: None,
                temperature: Some(0.2),
                top_p: Some(0.9),
                top_k: Some(40),
                presence_penalty: Some(0.1),
                frequency_penalty: Some(-0.5),
                max_tokens: Some(4_096),
                reasoning_effort: Some(ReasoningEffortLevel::Low),
                supports_tools: Some(false),
                supports_reasoning: None,
                supports_reasoning_effort: None,
                supports_vision: None,
                supports_structured_output: None,
                supports_parallel_tool_calls: None,
                supports_context_caching: None,
                supports_responses_compaction: None,
                supports_stream_usage: Some(false),
                supports_context_edits: None,
            },
        )]),
        request_policy: Default::default(),
    }
}

fn assert_custom_provider_round_trip(provider: &CustomProviderConfig) {
    assert_eq!(provider.name, "mycorp");
    assert_eq!(provider.display_name, "MyCorp");
    assert_eq!(provider.base_url, "https://llm.corp.example/v1");
    assert_eq!(provider.api_format, CustomProviderApiFormat::OpenAIChat);
    assert_eq!(provider.context_window, Some(256_000));
    assert_eq!(provider.supports_tools, Some(true));
    assert_eq!(provider.supports_reasoning, Some(false));
    assert_eq!(provider.supports_reasoning_effort, Some(true));
    assert_eq!(provider.supports_vision, Some(false));
    assert_eq!(provider.supports_structured_output, Some(true));
    assert_eq!(provider.supports_parallel_tool_calls, Some(false));
    assert_eq!(provider.supports_context_caching, Some(true));
    assert_eq!(provider.supports_responses_compaction, Some(false));
    assert_eq!(provider.supports_stream_usage, Some(true));
    assert_eq!(provider.supports_context_edits, Some(true));
    assert_eq!(provider.api_key_env, "MYCORP_API_KEY");
    assert_eq!(provider.model, "gpt-5-mini");
    assert_eq!(provider.models, vec!["gpt-5-mini".to_string(), "gpt-5-large".to_string()]);
    assert_eq!(
        provider.profiles["gpt-5-mini"],
        CustomProviderProfileConfig {
            api_format: CustomProviderApiFormat::OpenAIResponses,
            context_window: None,
            temperature: Some(0.2),
            top_p: Some(0.9),
            top_k: Some(40),
            presence_penalty: Some(0.1),
            frequency_penalty: Some(-0.5),
            max_tokens: Some(4_096),
            reasoning_effort: Some(ReasoningEffortLevel::Low),
            supports_tools: Some(false),
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: Some(false),
            supports_context_edits: None,
        }
    );
    let profile = &provider.profiles["gpt-5-mini"];
    assert_eq!(profile.temperature, Some(0.2));
    assert_eq!(profile.top_p, Some(0.9));
    assert_eq!(profile.top_k, Some(40));
    assert_eq!(profile.presence_penalty, Some(0.1));
    assert_eq!(profile.frequency_penalty, Some(-0.5));
    assert_eq!(profile.max_tokens, Some(4_096));
    assert_eq!(profile.reasoning_effort, Some(ReasoningEffortLevel::Low));
}

#[test]
fn custom_providers_fields_round_trip_through_toml() {
    let mut config = VTCodeConfig::default();
    config.custom_providers.push(custom_provider_round_trip_config());

    let serialized = toml::to_string(&config).expect("serialize config");
    let parsed: VTCodeConfig = toml::from_str(&serialized).expect("parse config");

    parsed.validate().expect("custom provider config should validate");
    assert_eq!(parsed.custom_providers.len(), 1);

    assert_custom_provider_round_trip(&parsed.custom_providers[0]);
}

#[test]
fn custom_providers_nested_profiles_parse_exactly() {
    let config: VTCodeConfig = toml::from_str(
        r#"
[[custom_providers]]
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.corp.example/v1"
api_format = "openai-chat"
context_window = 256000
supports_tools = true
supports_stream_usage = true

[custom_providers.profiles."gpt-5-mini"]
supports_tools = false
supports_parallel_tool_calls = true
supports_stream_usage = false
"#,
    )
    .expect("nested custom provider config should parse");

    let provider = config.custom_providers.first().expect("provider should exist");
    assert_eq!(provider.api_format, CustomProviderApiFormat::OpenAIChat);
    assert!(
        provider
            .profile("gpt-5-mini")
            .is_some_and(|profile| profile.supports_tools == Some(false)),
        "the model profile should disable tools"
    );
    assert!(provider.profile("gpt-5").is_none());
    assert_eq!(
        provider.resolved_profile("gpt-5-mini"),
        crate::core::ResolvedCustomProviderProfile {
            api_format: Some(CustomProviderApiFormat::OpenAIChat),
            context_window: Some(256_000),
            temperature: None,
            top_p: None,
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: None,
            supports_tools: Some(false),
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: Some(true),
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_stream_usage: Some(false),
            supports_context_edits: None,
        }
    );
}

#[test]
fn custom_providers_profile_sampling_overrides_resolve_with_provider_defaults() {
    let config: VTCodeConfig = toml::from_str(
        r#"
[[custom_providers]]
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.corp.example/v1"
temperature = 0.5
top_p = 0.95

[custom_providers.profiles."cold-model"]
temperature = 0.0
reasoning_effort = "low"

[custom_providers.profiles."hot-model"]
max_tokens = 8192
"#,
    )
    .expect("nested custom provider sampling overrides should parse");

    let provider = config.custom_providers.first().expect("provider should exist");
    let cold = provider.resolved_profile("cold-model");
    assert_eq!(cold.temperature, Some(0.0));
    // Provider-level top_p applies where the model profile does not override it.
    assert_eq!(cold.top_p, Some(0.95));
    assert_eq!(cold.reasoning_effort, Some(ReasoningEffortLevel::Low));
    assert_eq!(cold.max_tokens, None);

    let hot = provider.resolved_profile("hot-model");
    assert_eq!(hot.temperature, Some(0.5));
    assert_eq!(hot.max_tokens, Some(8_192));
}

#[test]
fn custom_providers_reject_out_of_range_sampling_overrides() {
    for (field, value) in [
        ("temperature", "2.5"),
        ("temperature", "-0.1"),
        ("top_p", "1.5"),
        ("presence_penalty", "-3.0"),
        ("frequency_penalty", "2.1"),
    ] {
        let config: Result<VTCodeConfig, _> = toml::from_str(&format!(
            r#"
[[custom_providers]]
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.corp.example/v1"

[custom_providers.profiles."m"]
{field} = {value}
"#
        ));
        let config = config.expect("config with out-of-range value should still parse");
        let error = config.custom_providers[0]
            .validate()
            .err()
            .unwrap_or_else(|| panic!("{field}={value} should fail validation"));
        assert!(error.contains(field), "validation error should mention {field}: {error}");
    }

    let zero_max: VTCodeConfig = toml::from_str(
        r#"
[[custom_providers]]
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.corp.example/v1"

[custom_providers.profiles."m"]
max_tokens = 0
"#,
    )
    .expect("zero max_tokens should parse");
    assert!(zero_max.validate().is_err(), "max_tokens=0 must fail validation");
}
