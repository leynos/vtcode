//! Factory registration tests for built-in providers and ad-hoc registrations.
use super::base_provider_config;
use crate::llm::cgp::CanDescribeProvider;
use crate::llm::factory::{LLMFactory, ProviderConfig, resolve_custom_provider_api_key};
use crate::llm::provider_config::{AnthropicProviderConfig, GeminiProviderConfig, OpenAIProviderConfig};
use crate::llm::providers::OllamaProvider;
use diapason_config::core::{AnthropicConfig, OpenAIConfig};

#[test]
fn blank_runtime_api_key_allows_command_auth() {
    assert_eq!(resolve_custom_provider_api_key(Some("   ".to_string()), true, "MISSING_KEY"), None);
    assert_eq!(
        resolve_custom_provider_api_key(Some("explicit-key".to_string()), true, "MISSING_KEY"),
        Some("explicit-key".to_string())
    );
}

#[test]
fn builtin_cgp_registration_exposes_expected_provider_keys() {
    let factory = LLMFactory::new();
    let mut providers = factory.list_providers();
    providers.sort();

    assert_eq!(
        providers,
        vec![
            "anthropic",
            "copilot",
            "deepseek",
            "evolink",
            "gemini",
            "huggingface",
            "llamacpp",
            "lmstudio",
            "merge-gateway",
            "meta",
            "mimo",
            "minimax",
            "mistral",
            "moonshot",
            "nvidia",
            "ollama",
            "ollama-cloud",
            "openai",
            "opencode-go",
            "opencode-zen",
            "openresponses",
            "openrouter",
            "poolside",
            "qwen",
            "stepfun",
            "xai",
            "zai",
        ]
    );
}

#[test]
fn standard_provider_builds_through_cgp_registration() {
    let factory = LLMFactory::new();
    let provider = factory
        .create_provider(
            <GeminiProviderConfig as CanDescribeProvider>::PROVIDER_KEY,
            ProviderConfig {
                api_key: Some("test-key".to_string()),
                model: Some(diapason_config::constants::models::google::GEMINI_3_FLASH_PREVIEW.to_string()),
                ..base_provider_config()
            },
            &[],
        )
        .expect("built-in cgp registration should build");

    assert_eq!(provider.name(), "gemini");
}

#[test]
fn openai_build_preserves_provider_specific_config_path() {
    let factory = LLMFactory::new();
    let provider = factory
        .create_provider(
            <OpenAIProviderConfig as CanDescribeProvider>::PROVIDER_KEY,
            ProviderConfig {
                api_key: Some("test-key".to_string()),
                model: Some(diapason_config::constants::models::openai::DEFAULT_MODEL.to_string()),
                openai: Some(OpenAIConfig { websocket_mode: true, ..OpenAIConfig::default() }),
                anthropic: Some(AnthropicConfig::default()),
                ..base_provider_config()
            },
            &[],
        )
        .expect("openai cgp registration should build");

    assert_eq!(provider.name(), "openai");
}

#[test]
fn anthropic_build_preserves_provider_specific_config_path() {
    let factory = LLMFactory::new();
    let provider = factory
        .create_provider(
            <AnthropicProviderConfig as CanDescribeProvider>::PROVIDER_KEY,
            ProviderConfig {
                api_key: Some("test-key".to_string()),
                model: Some(diapason_config::constants::models::anthropic::DEFAULT_MODEL.to_string()),
                anthropic: Some(AnthropicConfig {
                    count_tokens_enabled: true,
                    ..AnthropicConfig::default()
                }),
                ..base_provider_config()
            },
            &[],
        )
        .expect("anthropic cgp registration should build");

    assert_eq!(provider.name(), "anthropic");
}

#[test]
fn custom_provider_registration_still_coexists_with_cgp_builtins() {
    let mut factory = LLMFactory::new();
    factory.register_provider("custom-test", |_config| {
        Box::new(OllamaProvider::from_config(
            None,
            Some("gpt-oss:20b".to_string()),
            Some("http://localhost:11434".to_string()),
            None,
            None,
            None,
            None,
        ))
    });

    let custom = factory
        .create_provider("custom-test", base_provider_config(), &[])
        .expect("custom provider should still register");
    let builtin = factory
        .create_provider(
            "openai",
            ProviderConfig {
                api_key: Some("test-key".to_string()),
                model: Some(diapason_config::constants::models::openai::DEFAULT_MODEL.to_string()),
                ..base_provider_config()
            },
            &[],
        )
        .expect("builtin provider should still build");

    assert_eq!(custom.name(), "ollama");
    assert_eq!(builtin.name(), "openai");
}
