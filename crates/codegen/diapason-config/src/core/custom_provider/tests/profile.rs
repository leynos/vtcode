//! Profile resolution tests for custom provider configuration.
use std::collections::BTreeMap;

use crate::core::custom_provider::{
    CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig, CustomProviderRequestPolicyConfig,
    ResolvedCustomProviderProfile,
};

#[test]
fn effective_models_uses_models_list_when_present() {
    let config = CustomProviderConfig {
        name: "atlascloud".to_string(),
        display_name: "Atlas Cloud".to_string(),
        base_url: "https://api.atlascloud.ai/v1".to_string(),
        api_key_env: "ATLASCLOUD_API_KEY".to_string(),
        model: "deepseek-ai/deepseek-v4-flash".to_string(),
        models: vec![
            "deepseek-ai/deepseek-v4-flash".to_string(),
            "deepseek-ai/deepseek-v4-pro".to_string(),
            "deepseek-ai/DeepSeek-V3-0324".to_string(),
            "qwen/qwen3.6-35b-a3b".to_string(),
            "moonshotai/kimi-k2.7-code".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "zai-org/glm-5.2".to_string(),
            "minimaxai/minimax-m3".to_string(),
        ],
        ..CustomProviderConfig::default()
    };

    assert_eq!(
        config.effective_models(),
        vec![
            "deepseek-ai/deepseek-v4-flash".to_string(),
            "deepseek-ai/deepseek-v4-pro".to_string(),
            "deepseek-ai/DeepSeek-V3-0324".to_string(),
            "qwen/qwen3.6-35b-a3b".to_string(),
            "moonshotai/kimi-k2.7-code".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "zai-org/glm-5.2".to_string(),
            "minimaxai/minimax-m3".to_string(),
        ]
    );
}

#[test]
fn effective_models_falls_back_to_single_model_field() {
    let config = CustomProviderConfig {
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    };

    assert_eq!(config.effective_models(), vec!["gpt-5-mini".to_string()]);
}

#[test]
fn resolved_profile_prefers_exact_model_key() {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "gpt-5-mini".to_string(),
        CustomProviderProfileConfig {
            api_format: CustomProviderApiFormat::OpenAIResponses,
            context_window: Some(128_000),
            supports_tools: Some(true),
            ..CustomProviderProfileConfig::default()
        },
    );

    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.example/v1".to_string(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        context_window: Some(256_000),
        supports_tools: Some(true),
        supports_reasoning: Some(true),
        model: "gpt-5-mini".to_string(),
        profiles,
        ..CustomProviderConfig::default()
    };

    let resolved = config.resolved_profile("gpt-5-mini");
    assert_eq!(
        resolved,
        ResolvedCustomProviderProfile {
            api_format: Some(CustomProviderApiFormat::OpenAIResponses),
            context_window: Some(128_000),
            supports_tools: Some(true),
            supports_reasoning: Some(true),
            ..ResolvedCustomProviderProfile::default()
        }
    );
    assert!(config.profile("gpt-5").is_none());
}

#[test]
fn sparse_inheritance_preserves_provider_defaults() {
    let mut profiles = BTreeMap::new();
    profiles.insert(
        "gpt-5-mini".to_string(),
        CustomProviderProfileConfig {
            supports_stream_usage: Some(false),
            supports_tools: Some(false),
            supports_reasoning_effort: Some(true),
            ..CustomProviderProfileConfig::default()
        },
    );

    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: "https://llm.example/v1".to_string(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        context_window: Some(256_000),
        supports_tools: Some(true),
        supports_reasoning: Some(false),
        supports_vision: Some(true),
        supports_parallel_tool_calls: Some(true),
        supports_context_caching: Some(false),
        supports_stream_usage: Some(true),
        model: "gpt-5-mini".to_string(),
        profiles,
        ..CustomProviderConfig::default()
    };

    let resolved = config.resolved_profile("gpt-5-mini");
    assert_eq!(resolved.api_format, Some(CustomProviderApiFormat::OpenAIChat));
    assert_eq!(resolved.context_window, Some(256_000));
    assert_eq!(resolved.supports_tools, Some(false));
    assert_eq!(resolved.supports_reasoning, Some(false));
    assert_eq!(resolved.supports_reasoning_effort, Some(true));
    assert_eq!(resolved.supports_vision, Some(true));
    assert_eq!(resolved.supports_parallel_tool_calls, Some(true));
    assert_eq!(resolved.supports_context_caching, Some(false));
    assert_eq!(resolved.supports_responses_compaction, None);
    assert_eq!(resolved.supports_stream_usage, Some(false));
    assert_eq!(resolved.supports_context_edits, None);
}

#[test]
fn legacy_default_behaviour_retains_auto_and_empty_profiles() {
    let parsed: CustomProviderConfig = toml::from_str(
        r#"
name = "mycorp"
display_name = "MyCorp"
base_url = "https://llm.example/v1"
model = "gpt-5-mini"
"#,
    )
    .expect("legacy custom provider config should parse");

    assert_eq!(parsed.api_format, CustomProviderApiFormat::Auto);
    assert!(parsed.profiles.is_empty());
    assert_eq!(parsed.resolved_profile("gpt-5-mini"), ResolvedCustomProviderProfile::default());
    assert_eq!(parsed.request_policy, CustomProviderRequestPolicyConfig::default());
}
