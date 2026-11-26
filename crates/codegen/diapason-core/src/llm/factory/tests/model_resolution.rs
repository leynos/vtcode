//! Model-to-provider resolution tests for the factory.
use super::base_provider_config;
use crate::llm::factory::{LLMFactory, ProviderConfig, create_provider_for_model, create_provider_with_config};
use crate::llm::provider::LLMError;

/// A known model id resolves to the provider that serves it.
///
/// Every case is the same call and the same assertion, so the table is the
/// whole contract. The unknown-model and unknown-provider cases stay separate
/// tests: they assert different error shapes rather than a provider name.
#[rstest::rstest]
#[case::bare_minimax("MiniMax-M3", "minimax")]
#[case::mistral("mistral-large-2512", "mistral")]
#[case::nvidia("nvidia/nemotron-3-ultra-550b-a55b", "nvidia")]
#[case::openrouter_repo_id("openai/gpt-oss-20b", "openrouter")]
fn create_provider_for_known_model_uses_its_provider(#[case] model: &str, #[case] expected_provider: &str) {
    let provider = create_provider_for_model(model, "test-key".to_string(), None, None)
        .unwrap_or_else(|error| panic!("{model} should resolve to {expected_provider}: {error}"));

    assert_eq!(provider.name(), expected_provider);
}

#[test]
fn create_provider_with_config_registers_merge_gateway() {
    let provider = create_provider_with_config(
        "merge-gateway",
        ProviderConfig {
            api_key: Some("test-key".to_string()),
            model: Some("deepseek/deepseek-v4-pro".to_string()),
            ..base_provider_config()
        },
    )
    .expect("Merge Gateway should be registered in the factory");

    assert_eq!(provider.name(), "merge-gateway");
    assert_eq!(provider.backend_kind(), diapason_commons::llm::BackendKind::MergeGateway);
}

#[test]
fn create_provider_for_unknown_model_returns_error() {
    match create_provider_for_model("totally-unknown-model", "test-key".to_string(), None, None) {
        Err(LLMError::InvalidRequest { .. }) => {}
        Err(error) => panic!("expected invalid request error, got {error:?}"),
        Ok(_) => panic!("unknown models should remain rejected"),
    }
}

#[test]
fn unknown_provider_error_lists_available_providers() {
    let factory = LLMFactory::new();
    let error = match factory.create_provider("merge-api", base_provider_config(), &[]) {
        Err(error) => error,
        Ok(_) => panic!("unknown provider should be rejected"),
    };

    let message = format!("{error:?}");
    assert!(message.contains("Unknown provider: merge-api"), "{message}");
    assert!(message.contains("Available providers:"), "{message}");
    assert!(message.contains("merge-gateway"), "{message}");
}

#[test]
fn provider_lookup_is_case_insensitive() {
    let factory = LLMFactory::new();
    let provider = factory
        .create_provider(
            "OpenAI",
            ProviderConfig {
                api_key: Some("test-key".to_string()),
                model: Some(diapason_config::constants::models::openai::DEFAULT_MODEL.to_string()),
                ..base_provider_config()
            },
            &[],
        )
        .expect("provider lookup should be case-insensitive");

    assert_eq!(provider.name(), "openai");
}
