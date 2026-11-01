//! Regression tests for configured rate-limit header overrides on Anthropic-format custom providers.

use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMError, LLMProvider, LLMRequest, Message};
use vtcode_config::core::{AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, RateLimitHeaderConfig};
use vtcode_config::{TimeoutsConfig, constants::models};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn start_mock_server_or_skip() -> Option<MockServer> {
    match tokio::spawn(async { MockServer::start().await }).await {
        Ok(server) => Some(server),
        Err(error) if error.is_panic() => {
            let message = error
                .into_panic()
                .downcast_ref::<String>()
                .map_or_else(|| "unknown panic".to_string(), Clone::clone);
            if message.contains("Operation not permitted") || message.contains("PermissionDenied") {
                return None;
            }
            panic!("mock server should start: {message}");
        }
        Err(error) => panic!("mock server task should complete: {error}"),
    }
}

fn custom_provider_config(base_url: String, model: &str) -> CustomProviderConfig {
    CustomProviderConfig {
        name: "proxy-gateway".to_string(),
        display_name: "Proxy Gateway".to_string(),
        base_url,
        api_format: CustomProviderApiFormat::AnthropicMessages,
        model: model.to_string(),
        models: vec![model.to_string()],
        rate_limit_headers: RateLimitHeaderConfig {
            requests_limit_per_minute: Some("x-proxy-request-limit".to_string()),
            requests_remaining_per_minute: Some("x-proxy-request-remaining".to_string()),
            reset_after_seconds: Some("x-proxy-reset-after".to_string()),
            ..RateLimitHeaderConfig::default()
        },
        ..CustomProviderConfig::default()
    }
}

#[tokio::test]
async fn anthropic_custom_provider_maps_configured_rate_limit_header_overrides() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("request-id", "proxy_req_123")
                .insert_header("retry-after", "7")
                .insert_header("x-proxy-request-limit", "120")
                .insert_header("x-proxy-request-remaining", "42")
                .insert_header("x-proxy-reset-after", "2.5")
                .set_body_string(r#"{"type":"error","error":{"message":"rate limit reached"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let model = models::anthropic::DEFAULT_MODEL;
    let provider = CustomProviderBackendRouter::from_config(
        custom_provider_config(server.uri(), model),
        Some("fixture-key".to_string()),
        Some(model.to_string()),
        server.uri(),
        None,
        Some(TimeoutsConfig::default()),
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );

    let error = LLMProvider::generate(
        &provider,
        LLMRequest {
            model: model.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("429 should surface as a rate-limit error");

    let LLMError::RateLimit { metadata: Some(metadata) } = error else {
        panic!("expected custom Anthropic rate-limit metadata");
    };
    assert_eq!(metadata.status, Some(429));
    assert_eq!(metadata.retry_after.as_deref(), Some("7"));
    let rate_limit = metadata.rate_limit.expect("configured override headers should map");
    assert_eq!(rate_limit.requests_limit_per_minute, Some(120));
    assert_eq!(rate_limit.requests_remaining_per_minute, Some(42));
    assert_eq!(rate_limit.reset_after_millis, Some(2_500));
    assert_eq!(
        rate_limit.tokens_limit_per_minute, None,
        "native Anthropic header names must not be required when overrides are configured"
    );
}
