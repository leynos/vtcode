use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMProvider, LLMRequest, Message, SamplingOverrides};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use vtcode_config::constants::models;
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig,
};
use vtcode_config::types::ReasoningEffortLevel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sampling_router() -> CustomProviderBackendRouter {
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        "cold-model".to_string(),
        CustomProviderProfileConfig {
            temperature: Some(0.0),
            top_p: Some(0.9),
            reasoning_effort: Some(ReasoningEffortLevel::Low),
            ..Default::default()
        },
    );

    CustomProviderBackendRouter::from_config(
        CustomProviderConfig {
            name: "sampling-test".to_string(),
            display_name: "Sampling Test".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            temperature: Some(0.5),
            models: vec!["cold-model".to_string(), "warm-model".to_string()],
            profiles,
            ..Default::default()
        },
        None,
        None,
        "https://llm.example/v1".to_string(),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}

#[test]
fn sampling_overrides_resolve_per_model_with_provider_defaults() {
    let router = sampling_router();

    let cold = router.sampling_overrides("cold-model");
    assert_eq!(
        cold,
        SamplingOverrides {
            temperature: Some(0.0),
            top_p: Some(0.9),
            top_k: None,
            presence_penalty: None,
            frequency_penalty: None,
            max_tokens: None,
            reasoning_effort: Some(ReasoningEffortLevel::Low),
            suppresses_sampling_with_reasoning: false,
            profile_aware: true,
        }
    );
    assert!(router.supports_reasoning_effort("cold-model"));

    // Models without a profile fall back to provider-level defaults.
    let warm = router.sampling_overrides("warm-model");
    assert_eq!(warm.temperature, Some(0.5));
    assert_eq!(warm.top_p, None);
}

#[test]
fn sampling_overrides_suppression_matrix_matches_native_semantics() {
    let profile_openai = SamplingOverrides { profile_aware: true, ..Default::default() };
    let profile_anthropic = SamplingOverrides {
        suppresses_sampling_with_reasoning: true,
        profile_aware: true,
        ..Default::default()
    };
    let builtin_default = SamplingOverrides::default();

    // Custom openai-shaped profile keeps pinned values during reasoning.
    assert!(!profile_openai.suppresses_sampling(false, true));

    // Custom anthropic-messages profile drops them.
    assert!(profile_anthropic.suppresses_sampling(false, true));
    assert!(!profile_anthropic.suppresses_sampling(false, false));

    // Built-in Anthropic/MiniMax shape suppresses only via native match;
    // built-in OpenAI shape never does through overrides alone.
    assert!(builtin_default.suppresses_sampling(true, true));
    assert!(!builtin_default.suppresses_sampling(true, false));
    assert!(!builtin_default.suppresses_sampling(false, true));

    // Level "none"/"unknown" is not active reasoning anywhere.
    assert!(!profile_anthropic.suppresses_sampling(false, false));
}

#[test]
fn turn_scoped_system_capability_follows_selected_api_profile() {
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        models::anthropic::CLAUDE_FABLE_5.to_string(),
        CustomProviderProfileConfig {
            api_format: CustomProviderApiFormat::AnthropicMessages,
            ..Default::default()
        },
    );

    let router = CustomProviderBackendRouter::from_config(
        CustomProviderConfig {
            name: "profiled-transport-test".to_string(),
            display_name: "Profiled Transport Test".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            model: models::anthropic::CLAUDE_FABLE_5.to_string(),
            models: vec![
                models::anthropic::CLAUDE_FABLE_5.to_string(),
                "openai-shaped".to_string(),
            ],
            profiles,
            ..Default::default()
        },
        Some("test-key".to_string()),
        None,
        "https://llm.example/v1".to_string(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );

    assert!(router.supports_turn_scoped_system_messages(models::anthropic::CLAUDE_FABLE_5));
    assert!(!router.supports_turn_scoped_system_messages("openai-shaped"));
}

#[test]
fn anthropic_profile_accepts_turn_scoped_system_marker() {
    let mut profiles = std::collections::BTreeMap::new();
    profiles.insert(
        models::anthropic::CLAUDE_FABLE_5.to_string(),
        CustomProviderProfileConfig {
            api_format: CustomProviderApiFormat::AnthropicMessages,
            ..Default::default()
        },
    );
    let router = CustomProviderBackendRouter::from_config(
        CustomProviderConfig {
            name: "profiled-validation-test".to_string(),
            display_name: "Profiled Validation Test".to_string(),
            base_url: "https://llm.example/v1".to_string(),
            model: models::anthropic::CLAUDE_FABLE_5.to_string(),
            models: vec![models::anthropic::CLAUDE_FABLE_5.to_string()],
            profiles,
            ..Default::default()
        },
        Some("test-key".to_string()),
        None,
        "https://llm.example/v1".to_string(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let request = LLMRequest {
        model: models::anthropic::CLAUDE_FABLE_5.to_string(),
        messages: vec![Message::turn_scoped_system("visible-output notice".to_string())].into(),
        ..Default::default()
    };

    router
        .validate_request(&request)
        .expect("custom Anthropic profile should accept native marker");
}

#[tokio::test]
async fn openai_chat_custom_profile_preserves_reasoning_and_serialises_controls() {
    let server = MockServer::start().await;
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_for_mock = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            *captured_for_mock.lock().expect("capture mutex") = serde_json::from_slice(&request.body).ok();
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "content": "answer",
                        "reasoning_content": "think first"
                    },
                    "finish_reason": "stop"
                }]
            }))
        })
        .mount(&server)
        .await;

    let model = "DeepSeek-V4-Flash-0731".to_owned();
    let mut config = CustomProviderConfig {
        name: "deepseek-custom".to_owned(),
        display_name: "DeepSeek Custom".to_owned(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: model.clone(),
        models: vec![model.clone()],
        ..Default::default()
    };
    config.profiles.insert(
        model.clone(),
        CustomProviderProfileConfig {
            supports_reasoning: Some(true),
            supports_reasoning_effort: Some(true),
            ..Default::default()
        },
    );

    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("test-key".to_owned()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let response = provider
        .generate(LLMRequest {
            model,
            messages: vec![Message::user("hello".to_owned())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::High),
            ..Default::default()
        })
        .await
        .expect("custom OpenAI chat response should parse");

    assert_eq!(response.reasoning.as_deref(), Some("think first"));
    let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
    assert_eq!(payload["include_reasoning"], true);
    assert_eq!(payload["reasoning_effort"], "high");
}
