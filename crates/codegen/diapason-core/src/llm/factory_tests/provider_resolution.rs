//! Provider inference from model identifiers and factory lookup errors.

use super::super::*;
use super::provider_config;

use crate::llm::provider::LLMError;

#[test]
fn create_provider_for_bare_minimax_model_uses_minimax_provider() {
    let provider = create_provider_for_model("MiniMax-M3", "test-key".to_string(), None, None)
        .expect("bare minimax model should resolve to minimax provider");

    assert_eq!(provider.name(), "minimax");
}

#[test]
fn create_provider_for_mistral_model_uses_mistral_provider() {
    let provider = create_provider_for_model("mistral-large-2512", "test-key".to_string(), None, None)
        .expect("mistral models should resolve through mistral provider");

    assert_eq!(provider.name(), "mistral");
}

#[test]
fn create_provider_for_nvidia_model_uses_nvidia_provider() {
    let provider = create_provider_for_model("nvidia/nemotron-3-ultra-550b-a55b", "test-key".to_string(), None, None)
        .expect("NVIDIA model should resolve to NVIDIA provider");

    assert_eq!(provider.name(), "nvidia");
}

#[test]
fn create_provider_with_config_registers_merge_gateway() {
    let provider = create_provider_with_config(
        "merge-gateway",
        provider_config(Some("test-key"), Some("deepseek/deepseek-v4-pro")),
    )
    .expect("Merge Gateway should be registered in the factory");

    assert_eq!(provider.name(), "merge-gateway");
    assert_eq!(provider.backend_kind(), diapason_commons::llm::BackendKind::MergeGateway);
}

#[test]
fn create_provider_for_openai_repo_id_uses_openrouter_provider() {
    let provider = create_provider_for_model("openai/gpt-oss-20b", "test-key".to_string(), None, None)
        .expect("repo identifiers should preserve openrouter routing");

    assert_eq!(provider.name(), "openrouter");
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
    let error = match factory.create_provider("merge-api", provider_config(None, None), &[]) {
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
            provider_config(Some("test-key"), Some(diapason_config::constants::models::openai::DEFAULT_MODEL)),
            &[],
        )
        .expect("provider lookup should be case-insensitive");

    assert_eq!(provider.name(), "openai");
}
