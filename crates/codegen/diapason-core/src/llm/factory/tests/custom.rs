//! Custom-provider registration tests for the factory.
use super::{atlas_cloud_provider_config, base_provider_config, mixed_profile_provider_config};
use crate::llm::factory::{
    BUILTIN_PROVIDER_KEYS, ProviderConfig, create_provider_with_config, get_factory, register_custom_providers,
};
use diapason_config::core::{AnthropicConfig, CustomProviderConfig, OpenAIConfig};

/// The request config every mixed-profile case builds against.
///
/// The profile decides the backend, so each case passes only the model it wants
/// resolved; the key, both provider sections and the shared defaults are the
/// same everywhere.
fn mixed_request(model: &str) -> ProviderConfig {
    ProviderConfig {
        api_key: Some("test-key".to_string()),
        model: Some(model.to_string()),
        openai: Some(OpenAIConfig::default()),
        anthropic: Some(AnthropicConfig::default()),
        ..base_provider_config()
    }
}

#[test]
#[serial_test::serial(global_llm_factory)]
fn custom_openai_compatible_provider_uses_configured_display_name() {
    register_custom_providers(&[CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        api_key_env: "MYCORP_API_KEY".to_string(),
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    }]);

    let provider = create_provider_with_config(
        "mycorp",
        ProviderConfig {
            model: Some("gpt-5-mini".to_string()),
            openai: Some(OpenAIConfig::default()),
            ..base_provider_config()
        },
    )
    .expect("custom provider should register");

    assert_eq!(provider.name(), "mycorp");
    assert_eq!(provider.supported_models(), vec!["gpt-5-mini".to_string()]);

    register_custom_providers(&[]);
}

#[test]
#[serial_test::serial(global_llm_factory)]
fn custom_openai_compatible_provider_uses_configured_context_window() {
    register_custom_providers(&[CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorporateName".to_string(),
        base_url: "https://llm.corp.example/v1".to_string(),
        context_window: Some(256_000),
        api_key_env: "MYCORP_API_KEY".to_string(),
        model: "gpt-5-mini".to_string(),
        ..CustomProviderConfig::default()
    }]);

    let provider = create_provider_with_config(
        "mycorp",
        ProviderConfig {
            model: Some("gpt-5-mini".to_string()),
            openai: Some(OpenAIConfig::default()),
            ..base_provider_config()
        },
    )
    .expect("custom provider should register");

    assert_eq!(provider.effective_context_size("gpt-5-mini"), 256_000);

    register_custom_providers(&[]);
}

#[test]
#[serial_test::serial(global_llm_factory)]
fn custom_provider_backend_kind_and_identity_follow_selected_default_model() {
    register_custom_providers(&[mixed_profile_provider_config()]);

    let provider =
        create_provider_with_config("mixed", mixed_request("claude-sonnet-5")).expect("mixed provider should build");

    assert_eq!(provider.name(), "mixed");
    assert_eq!(provider.backend_kind(), diapason_commons::llm::BackendKind::Anthropic);

    register_custom_providers(&[]);
}

#[test]
#[serial_test::serial(global_llm_factory)]
fn custom_provider_profile_overrides_context_and_capabilities() {
    register_custom_providers(&[mixed_profile_provider_config()]);

    let provider =
        create_provider_with_config("mixed", mixed_request("gpt-5-mini")).expect("mixed provider should build");

    assert!(!provider.supports_tools("gpt-5-mini"));
    assert_eq!(provider.effective_context_size("gpt-5-mini"), 256_000);
    assert!(provider.supports_tools("claude-sonnet-5"));
    assert_eq!(provider.effective_context_size("claude-sonnet-5"), 512_000);

    register_custom_providers(&[]);
}

#[test]
#[serial_test::serial(global_llm_factory)]
fn custom_provider_validation_errors_use_custom_display_name() {
    register_custom_providers(&[mixed_profile_provider_config()]);

    let provider =
        create_provider_with_config("mixed", mixed_request("gpt-5-mini")).expect("mixed provider should build");

    let error = provider
        .validate_request(&crate::llm::provider::LLMRequest {
            messages: std::sync::Arc::new(Vec::new()),
            model: "gpt-5-mini".to_string(),
            ..Default::default()
        })
        .expect_err("empty request should be rejected");

    assert!(format!("{error:?}").contains("Mixed"));

    register_custom_providers(&[]);
}

/// Atlas Cloud is OpenAI-compatible and configured via `[[custom_providers]]`.
/// This test mirrors what `src/cli/dispatch/commands.rs` does for non-interactive
/// flows (`ask`, `review`, `benchmark`, …): register custom providers from
/// config before resolving the provider, then resolve it through the same
/// factory path the CLI uses, with `model: None` to exercise the
/// `default_model` fallback.
#[test]
#[serial_test::serial(global_llm_factory)]
fn atlas_cloud_registers_as_openai_compatible_custom_provider() {
    register_custom_providers(&[atlas_cloud_provider_config()]);

    let provider = create_provider_with_config(
        "atlascloud",
        ProviderConfig {
            model: None,
            openai: Some(OpenAIConfig::default()),
            ..base_provider_config()
        },
    )
    .expect("atlas cloud should resolve as an OpenAI-compatible custom provider");

    assert_eq!(provider.name(), "atlascloud");
    assert_eq!(
        provider.supported_models(),
        vec![
            "deepseek-ai/deepseek-v4-flash".to_string(),
            "deepseek-ai/deepseek-v4-pro".to_string(),
            "deepseek-ai/DeepSeek-V3-0324".to_string(),
            "deepseek-ai/DeepSeek-V3.1".to_string(),
            "deepseek-ai/deepseek-r1-0528".to_string(),
            "deepseek-ai/deepseek-ocr".to_string(),
            "qwen/qwen3.6-35b-a3b".to_string(),
            "qwen/qwen3.6-plus".to_string(),
            "qwen/qwen3.5-122b-a10b".to_string(),
            "qwen/qwen3.5-35b-a3b".to_string(),
            "qwen/qwen3-coder-next".to_string(),
            "qwen/qwen3.5-397b-a17b".to_string(),
            "qwen/qwen3-max-2026-01-23".to_string(),
            "qwen/qwen3-235b-a22b-thinking-2507".to_string(),
            "qwen/qwen3-30b-a3b-thinking-2507".to_string(),
            "qwen/qwen3-next-80b-a3b-thinking".to_string(),
            "qwen/qwen3-next-80b-a3b-instruct".to_string(),
            "moonshotai/kimi-k2.7-code".to_string(),
            "moonshotai/kimi-k2.6".to_string(),
            "moonshotai/kimi-k2.5".to_string(),
            "moonshotai/Kimi-K2-Thinking".to_string(),
            "moonshotai/Kimi-K2-Instruct".to_string(),
            "moonshotai/Kimi-K2-Instruct-0905".to_string(),
            "zai-org/glm-5.2".to_string(),
            "zai-org/glm-5.1".to_string(),
            "zai-org/glm-5v-turbo".to_string(),
            "zai-org/glm-5-turbo".to_string(),
            "zai-org/glm-4.7".to_string(),
            "minimaxai/minimax-m3".to_string(),
            "minimaxai/minimax-m2.7".to_string(),
            "minimaxai/minimax-m2.5".to_string(),
            "minimaxai/minimax-m2.1".to_string(),
            "kwaipilot/kat-coder-pro-v2".to_string(),
            "Alibaba-NLP/Tongyi-DeepResearch-30B-A3B".to_string(),
        ]
    );

    register_custom_providers(&[]);
}

/// Calling `register_custom_providers(&[])` must clear any previously
/// registered custom providers while leaving built-ins intact. This guards
/// the sync/replace contract that the CLI dispatch path depends on (so a
/// user removing Atlas Cloud from `config.toml` does not leave a stale
/// registration in the global factory).
#[test]
#[serial_test::serial(global_llm_factory)]
fn register_custom_providers_with_empty_input_clears_custom_but_keeps_builtins() {
    register_custom_providers(&[atlas_cloud_provider_config()]);

    {
        let factory = get_factory().lock().expect("factory lock");
        assert!(
            factory.list_providers().iter().any(|k| k == "atlascloud"),
            "custom provider should be registered before clearing"
        );
    }

    register_custom_providers(&[]);

    let factory = get_factory().lock().expect("factory lock");
    let providers = factory.list_providers();
    assert!(
        !providers.iter().any(|k| k == "atlascloud"),
        "custom provider should be unregistered after sync with empty input"
    );
    for builtin in BUILTIN_PROVIDER_KEYS {
        assert!(
            providers.iter().any(|k| k == builtin),
            "built-in provider {builtin} must survive custom-provider sync"
        );
    }
}
