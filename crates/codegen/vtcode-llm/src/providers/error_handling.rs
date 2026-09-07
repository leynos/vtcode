//! Centralized error handling for LLM providers
//! Eliminates duplicate error handling code across providers

use crate::error_display;
use crate::provider::{LLMError, LLMErrorMetadata};
use crate::providers::common::{extract_header, read_provider_error_body};
use reqwest::Response;
use reqwest::header::{HeaderMap, HeaderName};
use serde_json::Value;
use vtcode_commons::llm::RateLimitMetadata;
use vtcode_commons::sanitizer::sanitize_provider_diagnostic;
use vtcode_config::core::RateLimitHeaderConfig;

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
    rate_limit: Option<RateLimitMetadata>,
}

const MAX_RESET_AFTER_MILLIS: u64 = 86_400_000;

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
    let metadata = extract_response_metadata(response.headers(), &RateLimitHeaderConfig::for_provider_name("Gemini"));
    let error_text = read_provider_error_body(response).await;
    Err(parse_api_error_with_metadata("Gemini", status, &error_text, metadata))
}

/// Handle HTTP response errors for Anthropic provider
#[cold]
pub(crate) async fn handle_anthropic_http_error(
    response: Response,
    rate_limit_headers: &RateLimitHeaderConfig,
) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let metadata = extract_response_metadata(response.headers(), rate_limit_headers);
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
    let metadata =
        extract_response_metadata(response.headers(), &RateLimitHeaderConfig::for_provider_name(provider_name));
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
pub(crate) fn parse_api_error(provider_name: &str, status: reqwest::StatusCode, body: &str) -> LLMError {
    parse_api_error_with_metadata(provider_name, status, body, ApiResponseMetadata::default())
}

/// Parse an API error while retaining the standard `Retry-After` value and
/// only the explicitly mapped, numeric rate-limit headers.
#[cold]
pub(crate) fn parse_api_error_with_headers(
    provider_name: &str,
    status: reqwest::StatusCode,
    body: &str,
    headers: &HeaderMap,
    rate_limit_headers: &RateLimitHeaderConfig,
) -> LLMError {
    let metadata = extract_response_metadata(headers, rate_limit_headers);
    parse_api_error_with_metadata(provider_name, status, body, metadata)
}

#[cold]
fn parse_api_error_with_metadata(
    provider_name: &str,
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
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("authentication_error".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(diagnostic.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        402 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("insufficient balance: {error_message}")),
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("insufficient_balance".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(diagnostic.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        422 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid parameters: {error_message}")),
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("invalid_parameters".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(diagnostic.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        429 => LLMError::RateLimit {
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("rate_limit_error".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(error_message.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        400 if is_rate_limit_error(status_code, body) => LLMError::RateLimit {
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("quota_exceeded".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(error_message.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        400 => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid request: {error_message}")),
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    Some("invalid_request".to_string()),
                    response_metadata.request_id.clone(),
                    response_metadata.organization_id.clone(),
                    response_metadata.retry_after.clone(),
                    Some(diagnostic.clone()),
                )
                .with_rate_limit(response_metadata.rate_limit.clone()),
            ),
        },
        _ => LLMError::Provider {
            message: error_display::format_llm_error(provider_name, &format!("http {status}: {error_message}")),
            metadata: Some(
                LLMErrorMetadata::new(
                    provider_name,
                    Some(status_code),
                    None,
                    response_metadata.request_id,
                    response_metadata.organization_id,
                    response_metadata.retry_after,
                    Some(diagnostic),
                )
                .with_rate_limit(response_metadata.rate_limit),
            ),
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

fn extract_response_metadata(headers: &HeaderMap, rate_limit_headers: &RateLimitHeaderConfig) -> ApiResponseMetadata {
    ApiResponseMetadata {
        request_id: extract_header(headers, &["request-id", "x-request-id", "openai-request-id"]),
        organization_id: extract_header(
            headers,
            &["anthropic-organization-id", "openai-organization", "x-organization-id"],
        ),
        retry_after: extract_header(headers, &["retry-after"]),
        rate_limit: extract_rate_limit_metadata(headers, rate_limit_headers),
    }
}

pub(crate) fn error_metadata_from_headers(
    provider_name: &str,
    status: reqwest::StatusCode,
    body: &str,
    headers: &HeaderMap,
    rate_limit_headers: &RateLimitHeaderConfig,
) -> Box<LLMErrorMetadata> {
    let metadata = extract_response_metadata(headers, rate_limit_headers);
    LLMErrorMetadata::new(
        provider_name,
        Some(status.as_u16()),
        None,
        metadata.request_id,
        metadata.organization_id,
        metadata.retry_after,
        Some(body.to_string()),
    )
    .with_rate_limit(metadata.rate_limit)
}

fn extract_rate_limit_metadata(
    headers: &HeaderMap,
    header_config: &RateLimitHeaderConfig,
) -> Option<RateLimitMetadata> {
    let metadata = RateLimitMetadata {
        requests_limit_per_minute: extract_u64_header(headers, &header_config.requests_limit_per_minute),
        requests_remaining_per_minute: extract_u64_header(headers, &header_config.requests_remaining_per_minute),
        tokens_limit_per_minute: extract_u64_header(headers, &header_config.tokens_limit_per_minute),
        tokens_remaining_per_minute: extract_u64_header(headers, &header_config.tokens_remaining_per_minute),
        requests_limit_per_second: extract_u64_header(headers, &header_config.requests_limit_per_second),
        requests_remaining_per_second: extract_u64_header(headers, &header_config.requests_remaining_per_second),
        tokens_limit_per_second: extract_u64_header(headers, &header_config.tokens_limit_per_second),
        tokens_remaining_per_second: extract_u64_header(headers, &header_config.tokens_remaining_per_second),
        prompt_tokens_limit_per_second: extract_u64_header(headers, &header_config.prompt_tokens_limit_per_second),
        cache_adjusted_prompt_tokens_limit_per_second: extract_u64_header(
            headers,
            &header_config.cache_adjusted_prompt_tokens_limit_per_second,
        ),
        generated_tokens_limit_per_second: extract_u64_header(
            headers,
            &header_config.generated_tokens_limit_per_second,
        ),
        prompt_tokens: extract_u64_header(headers, &header_config.prompt_tokens),
        cached_prompt_tokens: extract_u64_header(headers, &header_config.cached_prompt_tokens),
        reset_after_millis: extract_reset_after_millis(headers, &header_config.reset_after_seconds),
    };

    (!metadata.is_empty()).then_some(metadata)
}

fn extract_u64_header(headers: &HeaderMap, configured_name: &Option<String>) -> Option<u64> {
    let name = HeaderName::from_bytes(configured_name.as_deref()?.as_bytes()).ok()?;
    let value = headers.get(name)?.to_str().ok()?.trim();
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn extract_reset_after_millis(headers: &HeaderMap, configured_name: &Option<String>) -> Option<u64> {
    let name = HeaderName::from_bytes(configured_name.as_deref()?.as_bytes()).ok()?;
    parse_reset_after_millis(headers.get(name)?.to_str().ok()?)
}

fn parse_reset_after_millis(raw_seconds: &str) -> Option<u64> {
    let raw_seconds = raw_seconds.trim();
    let (whole_seconds, fractional_seconds) = match raw_seconds.split_once('.') {
        Some((whole, fraction)) if !whole.is_empty() && !fraction.is_empty() && !fraction.contains('.') => {
            (whole, Some(fraction))
        }
        Some(_) => return None,
        None => (raw_seconds, None),
    };
    let whole_millis = whole_seconds.parse::<u64>().ok()?.checked_mul(1_000)?;
    let fractional_millis = fractional_seconds.map_or(Some(0), |fraction| {
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let milliseconds_digits = fraction.len().min(3);
        let mut milliseconds = fraction[..milliseconds_digits].parse::<u64>().ok()?;
        milliseconds *= 10_u64.pow(u32::try_from(3 - milliseconds_digits).ok()?);
        if fraction
            .as_bytes()
            .get(3..)
            .is_some_and(|tail| tail.iter().any(|digit| *digit != b'0'))
        {
            milliseconds += 1;
        }
        Some(milliseconds)
    })?;
    let millis = whole_millis.checked_add(fractional_millis)?;
    (millis <= MAX_RESET_AFTER_MILLIS).then_some(millis)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

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
    fn mapped_baseten_headers_and_retry_after_are_retained_without_raw_headers() {
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "7".parse().expect("static retry-after"));
        headers.insert("x-ratelimit-limit-requests", "120".parse().expect("numeric header"));
        headers.insert("x-ratelimit-remaining-requests", "3".parse().expect("numeric header"));
        headers.insert("x-ratelimit-limit-tokens", "50000".parse().expect("numeric header"));
        headers.insert("x-ratelimit-remaining-tokens", "400".parse().expect("numeric header"));
        headers.insert("x-secret-provider-note", "do-not-retain".parse().expect("static header"));

        let error = parse_api_error_with_headers(
            "Baseten",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            r#"{"error":{"message":"slow down"}}"#,
            &headers,
            &RateLimitHeaderConfig::default(),
        );
        let LLMError::RateLimit { metadata: Some(metadata) } = error else {
            panic!("expected rate-limit metadata");
        };
        assert_eq!(metadata.retry_after.as_deref(), Some("7"));
        let rate_limit = metadata.rate_limit.as_ref().expect("mapped rate-limit headers");
        assert_eq!(rate_limit.requests_limit_per_minute, Some(120));
        assert_eq!(rate_limit.requests_remaining_per_minute, Some(3));
        assert_eq!(rate_limit.tokens_limit_per_minute, Some(50_000));
        assert_eq!(rate_limit.tokens_remaining_per_minute, Some(400));
        let serialized = serde_json::to_string(&metadata).expect("metadata serialization");
        assert!(!serialized.contains("x-secret-provider-note"));
        assert!(!serialized.contains("do-not-retain"));
    }

    #[test]
    fn malformed_or_overflowing_rate_limit_headers_are_ignored() {
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-requests", "unlimited".parse().expect("static header"));
        headers.insert("x-ratelimit-remaining-requests", "+12".parse().expect("signed header"));
        headers.insert("x-ratelimit-limit-tokens", "18446744073709551616".parse().expect("overflowing numeric header"));

        assert_eq!(extract_rate_limit_metadata(&headers, &RateLimitHeaderConfig::default()), None);
    }

    #[test]
    fn fireworks_limits_and_request_counters_keep_distinct_semantics() {
        let config = RateLimitHeaderConfig::for_provider_name("fireworks-proxy");
        let mut headers = HeaderMap::new();
        headers.insert("x-ratelimit-limit-tokens-prompt", "60000".parse().expect("numeric header"));
        headers.insert("x-ratelimit-limit-tokens-cache-adjusted-prompt", "15000".parse().expect("numeric header"));
        headers.insert("x-ratelimit-limit-tokens-generated", "600".parse().expect("numeric header"));
        headers.insert("fireworks-prompt-tokens", "8000".parse().expect("numeric header"));
        headers.insert("fireworks-cached-prompt-tokens", "7500".parse().expect("numeric header"));

        let metadata = extract_rate_limit_metadata(&headers, &config).expect("Fireworks metadata");
        assert_eq!(metadata.prompt_tokens_limit_per_second, Some(60_000));
        assert_eq!(metadata.cache_adjusted_prompt_tokens_limit_per_second, Some(15_000));
        assert_eq!(metadata.generated_tokens_limit_per_second, Some(600));
        assert_eq!(metadata.prompt_tokens, Some(8_000));
        assert_eq!(metadata.cached_prompt_tokens, Some(7_500));
    }

    #[test]
    fn together_fractional_reset_rounds_up_without_overwriting_retry_after() {
        let config = RateLimitHeaderConfig::for_provider_name("Together");
        let mut headers = HeaderMap::new();
        headers.insert("retry-after", "2".parse().expect("static retry-after"));
        headers.insert("x-ratelimit-reset", "0.0001".parse().expect("fractional reset"));
        headers.insert("x-tokenlimit-limit", "2000".parse().expect("numeric header"));

        let metadata = extract_response_metadata(&headers, &config);
        assert_eq!(metadata.retry_after.as_deref(), Some("2"));
        let rate_limit = metadata.rate_limit.expect("Together metadata");
        assert_eq!(rate_limit.tokens_limit_per_second, Some(2_000));
        assert_eq!(rate_limit.reset_after_millis, Some(1));
    }

    proptest! {
        #[test]
        fn fractional_reset_never_rounds_below_wire_value(
            whole_seconds in 0_u64..86_400,
            fractional_millionths in 0_u32..1_000_000,
        ) {
            let wire_value = format!("{whole_seconds}.{fractional_millionths:06}");
            let parsed_millis = parse_reset_after_millis(&wire_value).expect("generated reset is in range");
            let exact_micros = whole_seconds * 1_000_000 + u64::from(fractional_millionths);

            prop_assert!(parsed_millis * 1_000 >= exact_micros);
            prop_assert!(parsed_millis * 1_000 < exact_micros + 1_000);
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
