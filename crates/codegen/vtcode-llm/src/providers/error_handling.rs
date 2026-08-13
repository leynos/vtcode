//! Centralized error handling for LLM providers
//! Eliminates duplicate error handling code across providers

use crate::error_display;
use crate::provider::{LLMError, LLMErrorMetadata};
use crate::providers::common::{extract_header, read_provider_error_body};
use reqwest::Response;
use serde_json::Value;
use vtcode_commons::sanitizer::sanitize_provider_diagnostic;

/// Stable classification for failures reported by reqwest.
///
/// The value is persisted in [`LLMErrorMetadata::code`] so callers can inspect
/// the transport failure without parsing reqwest's display text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReqwestErrorKind {
    Connect,
    Timeout,
    Request,
    Body,
    Decode,
    Redirect,
    Status,
    Unknown,
}

impl ReqwestErrorKind {
    const fn metadata_code(self) -> &'static str {
        match self {
            Self::Connect => "reqwest_connect_error",
            Self::Timeout => "reqwest_timeout_error",
            Self::Request => "reqwest_request_error",
            Self::Body => "reqwest_body_error",
            Self::Decode => "reqwest_decode_error",
            Self::Redirect => "reqwest_redirect_error",
            Self::Status => "reqwest_status_error",
            Self::Unknown => "reqwest_unknown_error",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct ReqwestErrorFlags {
    is_connect: bool,
    is_timeout: bool,
    is_request: bool,
    is_body: bool,
    is_decode: bool,
    is_redirect: bool,
    is_status: bool,
}

impl From<&reqwest::Error> for ReqwestErrorFlags {
    fn from(error: &reqwest::Error) -> Self {
        Self {
            is_connect: error.is_connect(),
            is_timeout: error.is_timeout(),
            is_request: error.is_request(),
            is_body: error.is_body(),
            is_decode: error.is_decode(),
            is_redirect: error.is_redirect(),
            is_status: error.is_status(),
        }
    }
}

const fn classify_reqwest_flags(flags: ReqwestErrorFlags) -> ReqwestErrorKind {
    // Some reqwest errors occupy more than one category. Prefer the most
    // actionable classification over the generic request/body buckets.
    if flags.is_timeout {
        ReqwestErrorKind::Timeout
    } else if flags.is_connect {
        ReqwestErrorKind::Connect
    } else if flags.is_decode {
        ReqwestErrorKind::Decode
    } else if flags.is_redirect {
        ReqwestErrorKind::Redirect
    } else if flags.is_status {
        ReqwestErrorKind::Status
    } else if flags.is_body {
        ReqwestErrorKind::Body
    } else if flags.is_request {
        ReqwestErrorKind::Request
    } else {
        ReqwestErrorKind::Unknown
    }
}

pub(crate) fn classify_reqwest_error(error: &reqwest::Error) -> ReqwestErrorKind {
    classify_reqwest_flags(error.into())
}

#[derive(Debug, Clone, Default)]
struct ApiResponseMetadata {
    request_id: Option<String>,
    organization_id: Option<String>,
    retry_after: Option<String>,
}

/// HTTP status codes for common error types
const STATUS_UNAUTHORIZED: u16 = 401;
const STATUS_FORBIDDEN: u16 = 403;
const STATUS_BAD_REQUEST: u16 = 400;
const STATUS_TOO_MANY_REQUESTS: u16 = 429;

/// Common rate limit error patterns (pre-lowercased for efficient matching)
const RATE_LIMIT_PATTERNS: &[&str] = &[
    "insufficient_quota",
    "resource_exhausted",
    "quota",
    "rate limit",
    "rate_limit",
    "ratelimit",
    "ratelimitexceeded",
    "concurrency",
    "frequency",
    "usage limit",
    "too many requests",
    "daily call limit",
    "package has expired",
];

/// Handle HTTP response errors for Gemini provider
#[cold]
pub async fn handle_gemini_http_error(response: Response) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let metadata = extract_response_metadata(&response);
    let error_text = read_provider_error_body(response).await;
    Err(parse_api_error_with_metadata("Gemini", status, &error_text, metadata))
}

/// Handle HTTP response errors for Anthropic provider
#[cold]
pub(crate) async fn handle_anthropic_http_error(response: Response) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let metadata = extract_response_metadata(&response);
    let error_text = read_provider_error_body(response).await;
    Err(parse_api_error_with_metadata("Anthropic", status, &error_text, metadata))
}

/// Handle HTTP response errors for OpenAI-compatible providers
#[cold]
pub(crate) async fn handle_openai_http_error(
    response: Response,
    provider_name: &'static str,
    _api_key_env_var: &str,
) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let metadata = extract_response_metadata(&response);
    let error_text = read_provider_error_body(response).await;

    // Universal diagnostic logging — helps debug post-tool follow-up failures
    // and transient API issues across all OpenAI-compatible providers.
    tracing::warn!(
        provider = provider_name,
        status = %status,
        body = %sanitize_provider_diagnostic(error_text.as_bytes()),
        "{} HTTP error",
        provider_name
    );

    Err(parse_api_error_with_metadata(provider_name, status, &error_text, metadata))
}

/// Check if an error is a rate limit error based on status code and message
#[cold]
pub(crate) fn is_rate_limit_error(status_code: u16, error_text: &str) -> bool {
    if status_code == STATUS_TOO_MANY_REQUESTS {
        return true;
    }

    // Optimize: Lowercase once and use pre-lowercased patterns
    let lower = error_text.to_lowercase();
    RATE_LIMIT_PATTERNS.iter().any(|pattern| lower.contains(pattern))
}

/// Handle network errors with consistent formatting
#[cold]
pub(crate) fn format_network_error(provider: &str, error: &impl std::fmt::Display) -> LLMError {
    let formatted_error = error_display::format_llm_error(provider, &format!("network error: {error}"));
    LLMError::Network { message: formatted_error, metadata: None }
}

/// Formats a reqwest transport failure while preserving its structured
/// classification and source chain in canonical LLM error metadata.
#[cold]
pub(crate) fn format_reqwest_network_error(provider: &str, error: &reqwest::Error) -> LLMError {
    let kind = classify_reqwest_error(error);
    let formatted_error = error_display::format_llm_error(provider, &format!("network error: {error}"));
    let source_chain = reqwest_source_chain(error);
    LLMError::Network {
        message: formatted_error,
        metadata: Some(LLMErrorMetadata::new(
            provider,
            error.status().map(|status| status.as_u16()),
            Some(kind.metadata_code().to_owned()),
            None,
            None,
            None,
            source_chain,
        )),
    }
}

fn reqwest_source_chain(error: &reqwest::Error) -> Option<String> {
    use std::error::Error as _;

    const MAX_SOURCE_DEPTH: usize = 8;

    let mut current = error.source();
    let mut depth = 0;
    let mut diagnostic = String::new();
    while let Some(source) = current {
        if !diagnostic.is_empty() {
            diagnostic.push_str(": ");
        }
        diagnostic.push_str(&source.to_string());
        current = source.source();
        depth += 1;
        if depth == MAX_SOURCE_DEPTH {
            break;
        }
    }

    if diagnostic.is_empty() {
        None
    } else {
        Some(sanitize_provider_diagnostic(diagnostic.as_bytes()))
    }
}

/// Handle JSON parsing errors with consistent formatting
#[cold]
pub(crate) fn format_parse_error(provider: &str, error: &impl std::fmt::Display) -> LLMError {
    let formatted_error = error_display::format_llm_error(provider, &format!("failed to parse response: {error}"));
    LLMError::Provider { message: formatted_error, metadata: None }
}

/// Format HTTP error with status code and message
#[cold]
pub fn format_http_error(provider: &str, status: reqwest::StatusCode, error_text: &str) -> String {
    error_display::format_llm_error(provider, &format!("http {status}: {error_text}"))
}

/// Parse standard API error response body into LLMError.
///
/// Handles multiple provider error formats:
/// - OpenAI/DeepSeek/ZAI: `{"error": {"message": "..."}}`
/// - Anthropic: `{"type": "error", "error": {"message": "..."}}`
/// - Gemini: `{"error": {"message": "...", "status": "..."}}`
/// - HuggingFace: `{"error": "..."}`
///
/// Falls back to raw body if JSON parsing fails.
#[cold]
pub(crate) fn parse_api_error(provider_name: &'static str, status: reqwest::StatusCode, body: &str) -> LLMError {
    parse_api_error_with_metadata(provider_name, status, body, ApiResponseMetadata::default())
}

#[cold]
fn parse_api_error_with_metadata(
    provider_name: &'static str,
    status: reqwest::StatusCode,
    body: &str,
    response_metadata: ApiResponseMetadata,
) -> LLMError {
    // Try to extract a meaningful error message from JSON
    let error_message = sanitize_provider_diagnostic(extract_human_error_message(body).as_bytes());
    let diagnostic = sanitize_provider_diagnostic(body.as_bytes());

    // Categorize by status code
    let status_code = status.as_u16();

    match status_code {
        401 | 403 => LLMError::Authentication {
            message: error_display::format_llm_error(
                provider_name,
                &authentication_error_message(provider_name, &error_message),
            ),
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("authentication_error".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(diagnostic.clone()),
            )),
        },
        402 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("insufficient balance: {error_message}")),
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("insufficient_balance".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(diagnostic.clone()),
            )),
        },
        422 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid parameters: {error_message}")),
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("invalid_parameters".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(diagnostic.clone()),
            )),
        },
        429 => LLMError::RateLimit {
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("rate_limit_error".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(error_message.clone()),
            )),
        },
        400 if is_rate_limit_error(status_code, body) => LLMError::RateLimit {
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("quota_exceeded".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(error_message.clone()),
            )),
        },
        400 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid request: {error_message}")),
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                Some("invalid_request".to_string()),
                response_metadata.request_id.clone(),
                response_metadata.organization_id.clone(),
                response_metadata.retry_after.clone(),
                Some(diagnostic.clone()),
            )),
        },
        _ => LLMError::Provider {
            message: error_display::format_llm_error(provider_name, &format!("http {status}: {error_message}")),
            metadata: Some(LLMErrorMetadata::new(
                provider_name,
                Some(status_code),
                None,
                response_metadata.request_id,
                response_metadata.organization_id,
                response_metadata.retry_after,
                Some(diagnostic),
            )),
        },
    }
}

fn authentication_error_message(provider_name: &str, error_message: &str) -> String {
    let trimmed = error_message.trim();
    if provider_name.eq_ignore_ascii_case("Moonshot") {
        return format!(
            "authentication failed: {trimmed}. get your API key from https://platform.kimi.ai/console/api-keys; Kimi web or app login credentials do not work for the API."
        );
    }

    if provider_name.eq_ignore_ascii_case("Qwen") {
        return format!(
            "authentication failed: {trimmed}. get your DashScope API key from https://dashscope.console.aliyun.com."
        );
    }

    if provider_name.eq_ignore_ascii_case("StepFun") {
        return format!("authentication failed: {trimmed}. get your API key from https://platform.stepfun.com.");
    }

    format!("authentication failed: {trimmed}")
}

/// Extract the most human-readable error message from a provider's JSON error body.
///
/// Handles all known provider response schemas:
/// - OpenAI/DeepSeek/ZAI/Anthropic: `{"error": {"message": "..."}}`
/// - HuggingFace: `{"error": "..."}`
/// - Gemini: `{"error": {"status": "..."}}`
/// - FastAPI / OpenAI alternate: `{"detail": "..."}`
/// - Generic: `{"message": "..."}`
///
/// Falls back to the raw body if no known field is found.
pub fn extract_human_error_message(body: &str) -> String {
    let Ok(json) = serde_json::from_str::<Value>(body) else {
        return body.to_string();
    };

    // OpenAI/DeepSeek/ZAI/Anthropic: {"error": {"message": "..."}}
    if let Some(msg) = json
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return msg.to_string();
    }
    // Mistral: {"object":"error","message":{"detail":[{"msg":"..."}]}}
    if let Some(detail) = json.get("message").and_then(|m| m.get("detail")).and_then(|d| d.as_array())
        && let Some(first) = detail.first().and_then(|d| d.get("msg")).and_then(|m| m.as_str())
    {
        return first.to_string();
    }
    // HuggingFace simple: {"error": "..."}
    if let Some(msg) = json.get("error").and_then(|e| e.as_str()).filter(|s| !s.trim().is_empty()) {
        return msg.to_string();
    }
    // FastAPI / OpenAI alternate: {"detail": "..."}
    if let Some(msg) = json.get("detail").and_then(|d| d.as_str()).filter(|s| !s.trim().is_empty()) {
        return msg.to_string();
    }
    // Gemini: {"error": {"status": "..."}}
    if let Some(msg) = json
        .get("error")
        .and_then(|e| e.get("status"))
        .and_then(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        return msg.to_string();
    }
    // Top-level message: {"message": "..."}
    if let Some(msg) = json.get("message").and_then(|m| m.as_str()).filter(|s| !s.trim().is_empty()) {
        return msg.to_string();
    }

    body.to_string()
}

fn extract_response_metadata(response: &Response) -> ApiResponseMetadata {
    let headers = response.headers();
    ApiResponseMetadata {
        request_id: extract_header(headers, &["request-id", "x-request-id", "openai-request-id"]),
        organization_id: extract_header(
            headers,
            &["anthropic-organization-id", "openai-organization", "x-organization-id"],
        ),
        retry_after: extract_header(headers, &["retry-after"]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reqwest_error_classification_covers_transport_categories() {
        let cases = [
            (ReqwestErrorFlags { is_connect: true, ..Default::default() }, ReqwestErrorKind::Connect),
            (ReqwestErrorFlags { is_timeout: true, ..Default::default() }, ReqwestErrorKind::Timeout),
            (ReqwestErrorFlags { is_request: true, ..Default::default() }, ReqwestErrorKind::Request),
            (ReqwestErrorFlags { is_body: true, ..Default::default() }, ReqwestErrorKind::Body),
            (ReqwestErrorFlags { is_decode: true, ..Default::default() }, ReqwestErrorKind::Decode),
            (ReqwestErrorFlags { is_redirect: true, ..Default::default() }, ReqwestErrorKind::Redirect),
            (ReqwestErrorFlags { is_status: true, ..Default::default() }, ReqwestErrorKind::Status),
            (ReqwestErrorFlags::default(), ReqwestErrorKind::Unknown),
        ];

        for (flags, expected) in cases {
            assert_eq!(classify_reqwest_flags(flags), expected);
        }
    }

    #[test]
    fn reqwest_error_classification_prefers_specific_categories() {
        let request_timeout = ReqwestErrorFlags {
            is_timeout: true,
            is_request: true,
            ..Default::default()
        };
        let request_connect = ReqwestErrorFlags {
            is_connect: true,
            is_request: true,
            ..Default::default()
        };
        let body_decode = ReqwestErrorFlags {
            is_body: true,
            is_decode: true,
            ..Default::default()
        };

        assert_eq!(classify_reqwest_flags(request_timeout), ReqwestErrorKind::Timeout);
        assert_eq!(classify_reqwest_flags(request_connect), ReqwestErrorKind::Connect);
        assert_eq!(classify_reqwest_flags(body_decode), ReqwestErrorKind::Decode);
    }

    #[test]
    fn reqwest_error_metadata_codes_are_stable() {
        assert_eq!(ReqwestErrorKind::Connect.metadata_code(), "reqwest_connect_error");
        assert_eq!(ReqwestErrorKind::Timeout.metadata_code(), "reqwest_timeout_error");
        assert_eq!(ReqwestErrorKind::Request.metadata_code(), "reqwest_request_error");
        assert_eq!(ReqwestErrorKind::Body.metadata_code(), "reqwest_body_error");
        assert_eq!(ReqwestErrorKind::Decode.metadata_code(), "reqwest_decode_error");
        assert_eq!(ReqwestErrorKind::Redirect.metadata_code(), "reqwest_redirect_error");
        assert_eq!(ReqwestErrorKind::Status.metadata_code(), "reqwest_status_error");
        assert_eq!(ReqwestErrorKind::Unknown.metadata_code(), "reqwest_unknown_error");
    }

    #[tokio::test]
    async fn reqwest_redirect_error_is_preserved_in_llm_metadata() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

        let server = MockServer::start().await;
        Mock::given(path("/loop"))
            .respond_with(ResponseTemplate::new(302).insert_header("location", format!("{}/loop", server.uri())))
            .mount(&server)
            .await;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(1))
            .build()
            .expect("redirect-limited test client should build");
        let error = client
            .post(format!("{}/loop", server.uri()))
            .send()
            .await
            .expect_err("redirect loop should exceed the configured limit");

        let llm_error = format_reqwest_network_error("Test provider", &error);
        match llm_error {
            LLMError::Network { metadata, .. } => {
                assert_eq!(metadata.as_ref().and_then(|value| value.code.as_deref()), Some("reqwest_redirect_error"));
            }
            other => panic!("expected a network error, got {other:?}"),
        }
    }

    #[test]
    fn test_rate_limit_detection() {
        assert!(is_rate_limit_error(429, ""));
        assert!(is_rate_limit_error(400, "insufficient_quota"));
        assert!(is_rate_limit_error(400, "RESOURCE_EXHAUSTED"));
        assert!(is_rate_limit_error(400, "rate limit exceeded"));
        assert!(!is_rate_limit_error(400, "invalid request"));
        assert!(!is_rate_limit_error(200, ""));
    }

    #[test]
    fn test_status_codes() {
        assert_eq!(STATUS_UNAUTHORIZED, 401);
        assert_eq!(STATUS_FORBIDDEN, 403);
        assert_eq!(STATUS_BAD_REQUEST, 400);
        assert_eq!(STATUS_TOO_MANY_REQUESTS, 429);
    }

    #[test]
    fn parse_openai_rate_limit_error_preserves_provider_message() {
        let error = parse_api_error(
            "OpenAI",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"Project rate limit exceeded for this model.","type":"rate_limit_error"}}"#,
        );

        match error {
            LLMError::RateLimit { metadata } => {
                assert_eq!(
                    metadata.as_ref().and_then(|meta| meta.message.as_deref()),
                    Some("Project rate limit exceeded for this model.")
                );
            }
            other => panic!("expected rate limit error, got {other:?}"),
        }
    }

    #[test]
    fn parse_moonshot_auth_error_includes_platform_key_guidance() {
        let error = parse_api_error(
            "Moonshot",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Invalid Authentication","type":"invalid_authentication_error"}}"#,
        );

        match error {
            LLMError::Authentication { message, metadata } => {
                assert!(message.contains("Invalid Authentication"));
                assert!(message.contains("platform.kimi.ai/console/api-keys"));
                assert!(!message.contains("/secret add"), "raw error should not embed CLI hint: {message}");
                assert_eq!(metadata.as_ref().and_then(|meta| meta.code.as_deref()), Some("authentication_error"));
            }
            other => panic!("expected authentication error, got {other:?}"),
        }
    }

    #[test]
    fn extract_openai_error_message() {
        let body = r#"{"error":{"message":"Model not found","type":"invalid_request_error"}}"#;
        assert_eq!(extract_human_error_message(body), "Model not found");
    }

    #[test]
    fn extract_detail_field() {
        let body = r#"{"detail":"The 'gpt-5.4' model is not supported with this method."}"#;
        assert_eq!(extract_human_error_message(body), "The 'gpt-5.4' model is not supported with this method.");
    }

    #[test]
    fn extract_huggingface_error_string() {
        let body = r#"{"error":"Model is currently loading"}"#;
        assert_eq!(extract_human_error_message(body), "Model is currently loading");
    }

    #[test]
    fn extract_top_level_message() {
        let body = r#"{"message":"Unauthorized access"}"#;
        assert_eq!(extract_human_error_message(body), "Unauthorized access");
    }

    #[test]
    fn extract_gemini_status() {
        let body = r#"{"error":{"status":"PERMISSION_DENIED","code":403}}"#;
        assert_eq!(extract_human_error_message(body), "PERMISSION_DENIED");
    }

    #[test]
    fn extract_falls_back_to_raw_body() {
        let body = "Internal Server Error";
        assert_eq!(extract_human_error_message(body), body);
    }

    #[test]
    fn extract_falls_back_for_unknown_json_schema() {
        let body = r#"{"code":500,"status":"error"}"#;
        assert_eq!(extract_human_error_message(body), body);
    }

    // --- authentication_error_message (via parse_api_error) ---

    #[test]
    fn stepfun_401_includes_platform_url_and_no_cli_hint() {
        let err = parse_api_error(
            "StepFun",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        match err {
            LLMError::Authentication { message, .. } => {
                assert!(message.contains("https://platform.stepfun.com"), "missing platform URL: {message}");
                assert!(!message.contains("/secret add"), "raw error should not embed CLI hint: {message}");
            }
            _ => panic!("expected Authentication error, got: {err:?}"),
        }
    }

    #[test]
    fn moonshot_401_includes_platform_url_and_no_cli_hint() {
        let err = parse_api_error(
            "Moonshot",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Invalid API key"}}"#,
        );
        match err {
            LLMError::Authentication { message, .. } => {
                assert!(
                    message.contains("https://platform.kimi.ai/console/api-keys"),
                    "missing platform URL: {message}"
                );
                assert!(!message.contains("/secret add"), "raw error should not embed CLI hint: {message}");
            }
            _ => panic!("expected Authentication error, got: {err:?}"),
        }
    }

    #[test]
    fn qwen_401_includes_platform_url_and_no_cli_hint() {
        let err =
            parse_api_error("Qwen", reqwest::StatusCode::UNAUTHORIZED, r#"{"error":{"message":"Invalid API key"}}"#);
        match err {
            LLMError::Authentication { message, .. } => {
                assert!(message.contains("https://dashscope.console.aliyun.com"), "missing platform URL: {message}");
                assert!(!message.contains("/secret add"), "raw error should not embed CLI hint: {message}");
            }
            _ => panic!("expected Authentication error, got: {err:?}"),
        }
    }

    #[test]
    fn openai_401_has_generic_auth_message() {
        let err = parse_api_error(
            "OpenAI",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        );
        match err {
            LLMError::Authentication { message, .. } => {
                assert!(message.contains("authentication failed"), "missing auth prefix: {message}");
                assert!(message.contains("Incorrect API key provided"));
            }
            _ => panic!("expected Authentication error, got: {err:?}"),
        }
    }
}
