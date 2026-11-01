//! Anthropic native error-metadata regression tests.

use crate::provider::{LLMError, LLMProvider, LLMRequest, Message};
use crate::providers::anthropic::AnthropicProvider;
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

#[tokio::test]
async fn native_anthropic_error_maps_numeric_headers_without_reset_timestamp() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("request-id", "anthropic_req_123")
                .insert_header("retry-after", "15")
                .insert_header("anthropic-ratelimit-requests-limit", "60")
                .insert_header("anthropic-ratelimit-requests-reset", "2026-09-08T12:00:00Z")
                .set_body_string(r#"{"type":"error","error":{"message":"rate limit reached"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let provider = AnthropicProvider::new_with_client(
        "test-key".to_string(),
        models::anthropic::DEFAULT_MODEL.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client should build"),
        server.uri(),
        TimeoutsConfig::default(),
    );
    let error = LLMProvider::generate(
        &provider,
        LLMRequest {
            model: models::anthropic::DEFAULT_MODEL.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("429 should surface as a rate-limit error");

    let LLMError::RateLimit { metadata: Some(metadata) } = error else {
        panic!("expected Anthropic rate-limit metadata");
    };
    assert_eq!(metadata.status, Some(429));
    assert_eq!(metadata.retry_after.as_deref(), Some("15"));
    let rate_limit = metadata.rate_limit.expect("configured numeric headers should map");
    assert_eq!(rate_limit.requests_limit_per_minute, Some(60));
    assert_eq!(rate_limit.reset_after_millis, None);
}
