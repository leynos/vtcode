//! Verifies OpenAI error metadata for buffered, streaming, and fallback paths.

use super::super::types::ResponsesApiState;
use super::tests::{start_mock_server_or_skip, test_provider};
use super::*;
use crate::provider::LLMProvider;
use rstest::rstest;
use serde_json::Value;
use vtcode_config::core::CustomProviderApiFormat;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn chat_completion_test_provider(base_url: &str, model: &str) -> OpenAIProvider {
    OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(model.to_string()),
        Some(base_url.to_string()),
        None,
        None,
        None,
        None,
        None,
        Some(vec![model.to_string()]),
    )
    .with_api_format_override(Some(CustomProviderApiFormat::OpenAIChat))
}

fn assert_header_metadata_without_provider_body(error: &provider::LLMError, request_id: &str) {
    let provider::LLMError::Provider { metadata: Some(metadata), .. } = error else {
        panic!("expected a provider error with header metadata: {error:?}");
    };
    assert_eq!(metadata.status, Some(400));
    assert_eq!(metadata.retry_after.as_deref(), Some("15"));
    assert_eq!(metadata.message, None);
    assert_eq!(
        metadata
            .rate_limit
            .as_ref()
            .and_then(|rate_limit| rate_limit.requests_limit_per_minute),
        Some(60)
    );

    let serialized = serde_json::to_value(metadata).expect("metadata should serialize");
    assert_eq!(serialized.get("request_id").and_then(Value::as_str), Some(request_id));
    assert!(!serialized.to_string().contains("customer-record=confidential-123"));
}

#[tokio::test]
async fn responses_requests_include_client_request_id_and_debug_metadata() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5);
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(|req: &wiremock::Request| {
            let req_id = req
                .headers
                .get("x-client-request-id")
                .and_then(|v| v.to_str().ok())
                .expect("x-client-request-id required");
            assert!(req_id.starts_with("vtcode-"));
            ResponseTemplate::new(400)
                .insert_header("x-request-id", "req_123")
                .insert_header("retry-after", "15")
                .insert_header("x-ratelimit-limit-requests", "60")
                .set_body_string(
                    r#"{"error":{"message":"customer-record=confidential-123","type":"invalid_request_error","param":"text.verbosity","code":"unsupported_parameter"}}"#,
                )
        })
        .expect(1)
        .mount(&server)
        .await;
    let err = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5.to_string(),
            ..Default::default()
        })
        .await
        .expect_err("should surface error");
    assert_header_metadata_without_provider_body(&err, "req_123");
    let text = err.to_string();
    assert!(
        text.contains("request_id=req_123")
            && text.contains("client_request_id=vtcode-")
            && text.contains("retry_after=15")
            && text.contains("type=invalid_request_error")
    );
}

#[derive(Clone, Copy)]
enum ResponsesStreamErrorPath {
    Legacy,
    Normalized,
}

impl ResponsesStreamErrorPath {
    const fn request_id(self) -> &'static str {
        match self {
            Self::Legacy => "req_stream_123",
            Self::Normalized => "req_normalized_123",
        }
    }

    const fn panic_message(self) -> &'static str {
        match self {
            Self::Legacy => "stream should surface the HTTP error",
            Self::Normalized => "normalized stream should surface the HTTP error",
        }
    }
}

async fn assert_responses_stream_error(error_path: ResponsesStreamErrorPath) {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5);
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-request-id", error_path.request_id())
                .insert_header("retry-after", "15")
                .insert_header("x-ratelimit-limit-requests", "60")
                .set_body_string(r#"{"error":{"message":"customer-record=confidential-123"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_string())].into(),
        model: models::openai::GPT_5.to_string(),
        ..Default::default()
    };
    let error = match error_path {
        ResponsesStreamErrorPath::Legacy => match provider.stream(request).await {
            Err(error) => error,
            Ok(_) => panic!("{}", error_path.panic_message()),
        },
        ResponsesStreamErrorPath::Normalized => match provider.stream_normalized(request).await {
            Err(error) => error,
            Ok(_) => panic!("{}", error_path.panic_message()),
        },
    };

    assert_header_metadata_without_provider_body(&error, error_path.request_id());
}

#[rstest]
#[case::legacy(ResponsesStreamErrorPath::Legacy)]
#[case::normalized(ResponsesStreamErrorPath::Normalized)]
#[tokio::test]
async fn responses_stream_errors_keep_header_metadata_without_provider_body(
    #[case] error_path: ResponsesStreamErrorPath,
) {
    assert_responses_stream_error(error_path).await;
}

#[tokio::test]
async fn chat_buffered_and_stream_errors_keep_header_metadata_without_provider_body() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = chat_completion_test_provider(&server.uri(), models::openai::GPT_5_6);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-request-id", "req_chat_123")
                .insert_header("retry-after", "15")
                .insert_header("x-ratelimit-limit-requests", "60")
                .set_body_string(r#"{"error":{"message":"customer-record=confidential-123"}}"#),
        )
        .expect(2)
        .mount(&server)
        .await;
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user("Hello".to_string())].into(),
        model: models::openai::GPT_5_6.to_string(),
        ..Default::default()
    };

    let buffered_error = provider.generate(request.clone()).await.expect_err("chat generate should fail");
    assert_header_metadata_without_provider_body(&buffered_error, "req_chat_123");

    let stream_error = match provider.stream(request).await {
        Err(error) => error,
        Ok(_) => panic!("chat stream should fail"),
    };
    assert_header_metadata_without_provider_body(&stream_error, "req_chat_123");
}

#[tokio::test]
async fn responses_fallback_reports_terminal_chat_header_metadata() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let model = models::openai::GPT_5_6;
    let provider = test_provider(&server.uri(), model);
    assert_eq!(provider.responses_api_state(model), ResponsesApiState::Allowed);
    assert!(provider.allows_chat_completions_fallback());
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Responses API is not supported"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(400)
                .insert_header("x-request-id", "req_fallback_chat_123")
                .insert_header("retry-after", "15")
                .insert_header("x-ratelimit-limit-requests", "60")
                .set_body_string(r#"{"error":{"message":"customer-record=confidential-123"}}"#),
        )
        .expect(1)
        .mount(&server)
        .await;

    let error = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: model.to_string(),
            ..Default::default()
        })
        .await
        .expect_err("terminal Chat fallback error should surface");

    assert_header_metadata_without_provider_body(&error, "req_fallback_chat_123");
}
