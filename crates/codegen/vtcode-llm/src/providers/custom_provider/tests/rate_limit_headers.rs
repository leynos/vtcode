//! Custom provider error metadata survives both API transports.
use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMError, LLMProvider, LLMRequest, Message};
use serde_json::json;
use vtcode_config::core::{AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, RateLimitHeaderConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
fn assert_custom_rate_limit(error: LLMError) {
    let LLMError::RateLimit { metadata: Some(metadata) } = error else {
        panic!("expected custom provider rate-limit metadata");
    };
    assert_eq!(metadata.retry_after.as_deref(), Some("4"));
    let rate_limit = metadata.rate_limit.expect("configured rate-limit headers");
    assert_eq!(rate_limit.requests_remaining_per_minute, Some(2));
    assert_eq!(rate_limit.tokens_limit_per_second, Some(900));
    assert_eq!(rate_limit.reset_after_millis, Some(1_250));
}

#[tokio::test]
async fn openai_chat_streaming_and_buffered_errors_preserve_configured_rate_limit_headers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "4")
                .insert_header("x-corp-requests-remaining", "2")
                .insert_header("x-corp-token-limit", "900")
                .insert_header("x-corp-reset", "1.25")
                .insert_header("x-corp-private", "must-not-escape")
                .set_body_json(json!({"error": {"message": "slow down"}})),
        )
        .expect(2)
        .mount(&server)
        .await;

    let model = "corp-model".to_string();
    let config = CustomProviderConfig {
        name: "mycorp".to_string(),
        display_name: "MyCorp".to_string(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: model.clone(),
        models: vec![model.clone()],
        rate_limit_headers: RateLimitHeaderConfig {
            requests_remaining_per_minute: Some("x-corp-requests-remaining".to_string()),
            tokens_limit_per_second: Some("x-corp-token-limit".to_string()),
            reset_after_seconds: Some("x-corp-reset".to_string()),
            ..RateLimitHeaderConfig::default()
        },
        ..Default::default()
    };
    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("fixture-key".to_string()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let request = || LLMRequest {
        model: model.clone(),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let stream_error = match provider.stream(request()).await {
        Ok(_) => panic!("streaming request should be limited"),
        Err(error) => error,
    };
    assert_custom_rate_limit(stream_error);
    assert_custom_rate_limit(
        provider
            .generate(request())
            .await
            .expect_err("buffered request should be limited"),
    );
}

#[tokio::test]
async fn openai_chat_retryable_provider_error_always_preserves_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(503)
                .insert_header("retry-after", "6")
                .set_body_json(json!({"error": {"message": "temporarily unavailable"}})),
        )
        .mount(&server)
        .await;

    let model = "corp-model".to_string();
    let provider = CustomProviderBackendRouter::from_config(
        CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: model.clone(),
            models: vec![model.clone()],
            rate_limit_headers: RateLimitHeaderConfig {
                prompt_tokens: Some("x-unrelated-counter".to_string()),
                ..RateLimitHeaderConfig::default()
            },
            ..Default::default()
        },
        Some("fixture-key".to_string()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );

    let error = provider
        .generate(LLMRequest {
            model,
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect_err("503 should remain a provider error");
    let LLMError::Provider { metadata: Some(metadata), .. } = error else {
        panic!("expected provider error metadata");
    };
    assert_eq!(metadata.retry_after.as_deref(), Some("6"));
    assert!(metadata.rate_limit.is_none());
}
