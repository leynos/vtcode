//! Centralized error handling for LLM providers
//! Eliminates duplicate error handling code across providers

use crate::error_display;
use crate::provider::{LLMError, LLMErrorMetadata};
use crate::providers::common::read_provider_error_body;
use reqwest::Response;
use reqwest::header::HeaderMap;
use serde_json::Value;
use vtcode_commons::llm::RateLimitMetadata;
use vtcode_commons::sanitizer::sanitize_provider_diagnostic;
use vtcode_config::core::RateLimitHeaderConfig;

mod rate_limit_headers;

use self::rate_limit_headers::extract_response_metadata;

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

/// The transport details of a failed provider response.
///
/// Grouping the status, body, and headers keeps call sites from threading the
/// same three values through every parse and metadata helper.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderFailure<'a> {
    pub(crate) status: reqwest::StatusCode,
    pub(crate) body: &'a str,
    pub(crate) headers: &'a HeaderMap,
}

impl<'a> ProviderFailure<'a> {
    pub(crate) const fn new(status: reqwest::StatusCode, body: &'a str, headers: &'a HeaderMap) -> Self {
        Self { status, body, headers }
    }
}

/// A failed provider response together with the identity and header mapping
/// needed to build canonical [`LLMErrorMetadata`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct ProviderHttpError<'a> {
    pub(crate) failure: ProviderFailure<'a>,
    pub(crate) provider_name: &'a str,
    pub(crate) rate_limit_headers: &'a RateLimitHeaderConfig,
}

impl<'a> ProviderHttpError<'a> {
    pub(crate) const fn new(
        failure: ProviderFailure<'a>,
        provider_name: &'a str,
        rate_limit_headers: &'a RateLimitHeaderConfig,
    ) -> Self {
        Self { failure, provider_name, rate_limit_headers }
    }
}

/// Category assigned to a failed provider HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApiErrorKind {
    Authentication,
    InsufficientBalance,
    InvalidParameters,
    RateLimit,
    QuotaExceeded,
    InvalidRequest,
    Provider,
}

impl ApiErrorKind {
    fn classify(failure: ProviderFailure<'_>) -> Self {
        match failure.status.as_u16() {
            STATUS_UNAUTHORIZED | STATUS_FORBIDDEN => Self::Authentication,
            402 => Self::InsufficientBalance,
            422 => Self::InvalidParameters,
            STATUS_TOO_MANY_REQUESTS => Self::RateLimit,
            STATUS_BAD_REQUEST if is_rate_limit_error(STATUS_BAD_REQUEST, failure.body) => Self::QuotaExceeded,
            STATUS_BAD_REQUEST => Self::InvalidRequest,
            _ => Self::Provider,
        }
    }

    /// Metadata code recorded for this category, when one is defined.
    const fn metadata_code(self) -> Option<&'static str> {
        match self {
            Self::Authentication => Some("authentication_error"),
            Self::InsufficientBalance => Some("insufficient_balance"),
            Self::InvalidParameters => Some("invalid_parameters"),
            Self::RateLimit => Some("rate_limit_error"),
            Self::QuotaExceeded => Some("quota_exceeded"),
            Self::InvalidRequest => Some("invalid_request"),
            Self::Provider => None,
        }
    }

    /// Diagnostic text retained in metadata for this category.
    ///
    /// Rate-limit categories keep the human-readable provider message; other
    /// categories keep the sanitised response diagnostic.
    const fn metadata_detail<'a>(self, detail: &'a ErrorDetail<'a>) -> &'a str {
        match self {
            Self::RateLimit | Self::QuotaExceeded => detail.message,
            Self::Authentication
            | Self::InsufficientBalance
            | Self::InvalidParameters
            | Self::InvalidRequest
            | Self::Provider => detail.diagnostic,
        }
    }
}

/// Sanitised text derived from one provider error body.
struct ErrorDetail<'a> {
    message: &'a str,
    diagnostic: &'a str,
}

/// HTTP status codes for common error types
const STATUS_UNAUTHORIZED: u16 = 401;
const STATUS_FORBIDDEN: u16 = 403;
const STATUS_BAD_REQUEST: u16 = 400;
const STATUS_TOO_MANY_REQUESTS: u16 = 429;

// Gemini does not expose the OpenAI-compatible quota headers. Keep the
// mapping explicit so a future default mapping cannot project another
// provider's headers as Gemini quota metadata.
pub(crate) const NO_RATE_LIMIT_HEADERS: RateLimitHeaderConfig = RateLimitHeaderConfig {
    requests_limit_per_minute: None,
    requests_remaining_per_minute: None,
    tokens_limit_per_minute: None,
    tokens_remaining_per_minute: None,
    requests_limit_per_second: None,
    requests_remaining_per_second: None,
    tokens_limit_per_second: None,
    tokens_remaining_per_second: None,
    prompt_tokens_limit_per_second: None,
    cache_adjusted_prompt_tokens_limit_per_second: None,
    generated_tokens_limit_per_second: None,
    prompt_tokens: None,
    cached_prompt_tokens: None,
    reset_after_seconds: None,
};

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

/// Convert a non-success provider response into an [`LLMError`].
///
/// Shared by every provider that maps documented rate-limit headers, so the
/// metadata-preserving path cannot drift between providers.
#[cold]
pub(crate) async fn handle_provider_http_error(
    response: Response,
    provider_name: &'static str,
    rate_limit_headers: &RateLimitHeaderConfig,
) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    let status = response.status();
    let headers = response.headers().clone();
    let error_text = read_provider_error_body(response).await;
    Err(parse_api_error(ProviderHttpError::new(
        ProviderFailure::new(status, &error_text, &headers),
        provider_name,
        rate_limit_headers,
    )))
}

/// Handle HTTP response errors for OpenAI-compatible providers.
#[cold]
pub(crate) async fn handle_openai_http_error(
    response: Response,
    provider_name: &'static str,
) -> Result<Response, LLMError> {
    if response.status().is_success() {
        return Ok(response);
    }

    // Keep response bodies out of logs: providers may include customer prompts
    // or other sensitive content in their error messages.
    tracing::warn!(
        provider = provider_name,
        status = %response.status(),
        "{} HTTP error",
        provider_name
    );

    let rate_limit_headers = RateLimitHeaderConfig::for_provider_name(provider_name);
    handle_provider_http_error(response, provider_name, &rate_limit_headers).await
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
pub(crate) fn parse_api_error(error: ProviderHttpError<'_>) -> LLMError {
    let metadata = extract_response_metadata(error.failure.headers, error.rate_limit_headers);
    parse_api_error_with_metadata(error, metadata)
}

#[cold]
fn parse_api_error_with_metadata(error: ProviderHttpError<'_>, response_metadata: ApiResponseMetadata) -> LLMError {
    let ProviderHttpError { failure, provider_name, .. } = error;
    // Try to extract a meaningful error message from JSON
    let error_message = sanitize_provider_diagnostic(extract_human_error_message(failure.body).as_bytes());
    let diagnostic = sanitize_provider_diagnostic(failure.body.as_bytes());
    let detail = ErrorDetail { message: &error_message, diagnostic: &diagnostic };
    let status_code = failure.status.as_u16();
    let kind = ApiErrorKind::classify(failure);

    let metadata = LLMErrorMetadata::new(
        provider_name,
        Some(status_code),
        kind.metadata_code().map(str::to_owned),
        response_metadata.request_id,
        response_metadata.organization_id,
        response_metadata.retry_after,
        Some(kind.metadata_detail(&detail).to_owned()),
    )
    .with_rate_limit(response_metadata.rate_limit);

    match kind {
        ApiErrorKind::Authentication => LLMError::Authentication {
            message: error_display::format_llm_error(
                provider_name,
                &authentication_error_message(provider_name, &error_message),
            ),
            metadata: Some(metadata),
        },
        ApiErrorKind::InsufficientBalance => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("insufficient balance: {error_message}")),
            metadata: Some(metadata),
        },
        ApiErrorKind::InvalidParameters => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid parameters: {error_message}")),
            metadata: Some(metadata),
        },
        ApiErrorKind::RateLimit | ApiErrorKind::QuotaExceeded => LLMError::RateLimit { metadata: Some(metadata) },
        ApiErrorKind::InvalidRequest => LLMError::InvalidRequest {
            message: error_display::format_llm_error(provider_name, &format!("invalid request: {error_message}")),
            metadata: Some(metadata),
        },
        ApiErrorKind::Provider => LLMError::Provider {
            message: error_display::format_llm_error(
                provider_name,
                &format!("http {}: {error_message}", failure.status),
            ),
            metadata: Some(metadata),
        },
    }
}

fn authentication_error_message(provider_name: &str, error_message: &str) -> String {
    let trimmed = error_message.trim();
    if provider_name.eq_ignore_ascii_case("Gemini") {
        return format!(
            "authentication failed: {trimmed}. check your GOOGLE_API_KEY or GEMINI_API_KEY environment variable."
        );
    }

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

pub(crate) fn error_metadata_from_headers(error: ProviderHttpError<'_>) -> Box<LLMErrorMetadata> {
    let metadata = extract_response_metadata(error.failure.headers, error.rate_limit_headers);
    LLMErrorMetadata::new(
        error.provider_name,
        Some(error.failure.status.as_u16()),
        None,
        metadata.request_id,
        metadata.organization_id,
        metadata.retry_after,
        None,
    )
    .with_rate_limit(metadata.rate_limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write as _;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing::{Event, Metadata, Subscriber};

    /// Parse an error body that has no mapped rate-limit headers.
    fn parse_provider_error(provider_name: &str, status: reqwest::StatusCode, body: &str) -> LLMError {
        let headers = HeaderMap::new();
        parse_api_error(ProviderHttpError::new(
            ProviderFailure::new(status, body, &headers),
            provider_name,
            &NO_RATE_LIMIT_HEADERS,
        ))
    }

    struct CapturedEvents {
        events: Arc<Mutex<Vec<String>>>,
        next_span_id: AtomicU64,
    }

    impl Subscriber for CapturedEvents {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
            Id::from_u64(self.next_span_id.fetch_add(1, Ordering::Relaxed) + 1)
        }

        fn record(&self, _span: &Id, _values: &Record<'_>) {}

        fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

        fn event(&self, event: &Event<'_>) {
            let mut fields = String::new();
            event.record(&mut CapturedFields(&mut fields));
            self.events
                .lock()
                .expect("captured event lock should be available")
                .push(fields);
        }

        fn enter(&self, _span: &Id) {}

        fn exit(&self, _span: &Id) {}
    }

    struct CapturedFields<'a>(&'a mut String);

    impl Visit for CapturedFields<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            let _ = writeln!(self.0, "{}={value:?}", field.name());
        }
    }

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
        let cases = [
            (ReqwestErrorKind::Connect, "reqwest_connect_error"),
            (ReqwestErrorKind::Timeout, "reqwest_timeout_error"),
            (ReqwestErrorKind::Request, "reqwest_request_error"),
            (ReqwestErrorKind::Body, "reqwest_body_error"),
            (ReqwestErrorKind::Decode, "reqwest_decode_error"),
            (ReqwestErrorKind::Redirect, "reqwest_redirect_error"),
            (ReqwestErrorKind::Status, "reqwest_status_error"),
            (ReqwestErrorKind::Unknown, "reqwest_unknown_error"),
        ];

        for (kind, expected_code) in cases {
            assert_eq!(kind.metadata_code(), expected_code);
        }
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
    fn openai_http_error_logs_metadata_without_provider_response_text() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

        const RESPONSE_MARKER: &str = "CUSTOMER_RESPONSE_MARKER_63aebd88";
        let captured_events = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing::Dispatch::new(CapturedEvents {
            events: Arc::clone(&captured_events),
            next_span_id: AtomicU64::default(),
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("current-thread runtime should build");

        let error = tracing::dispatcher::with_default(&subscriber, || {
            runtime.block_on(async {
                let server = MockServer::start().await;
                Mock::given(path("/error"))
                    .respond_with(
                        ResponseTemplate::new(500)
                            .set_body_string(format!(r#"{{"error":{{"message":"{RESPONSE_MARKER}"}}}}"#)),
                    )
                    .mount(&server)
                    .await;
                let response = reqwest::Client::new()
                    .post(format!("{}/error", server.uri()))
                    .send()
                    .await
                    .expect("mock provider response should be received");

                handle_openai_http_error(response, "Test Provider")
                    .await
                    .expect_err("HTTP error response should be returned as an LLM error")
            })
        });

        assert!(error.to_string().contains(RESPONSE_MARKER), "returned error should retain provider detail");
        let captured_events = captured_events.lock().expect("captured event lock should be available");
        let logs = captured_events.join("\n");
        assert!(logs.contains("Test Provider"), "provider metadata should remain in logs: {logs}");
        assert!(logs.contains("500"), "HTTP status should remain in logs: {logs}");
        assert!(!logs.contains(RESPONSE_MARKER), "provider response detail leaked into logs: {logs}");
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
        let error = parse_provider_error(
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
    fn error_metadata_from_headers_does_not_retain_provider_body() {
        let provider_body = r#"{"error":{"message":"customer-record=confidential-123"}}"#;
        let headers = HeaderMap::new();
        let metadata = error_metadata_from_headers(ProviderHttpError::new(
            ProviderFailure::new(reqwest::StatusCode::BAD_GATEWAY, provider_body, &headers),
            "OpenAI",
            &NO_RATE_LIMIT_HEADERS,
        ));

        assert_eq!(metadata.message, None);
        let serialized = serde_json::to_string(&metadata).expect("metadata should serialize");
        assert!(!serialized.contains("customer-record=confidential-123"));
    }

    #[tokio::test]
    async fn gemini_http_errors_keep_retry_after_without_baseten_quota_metadata() {
        use wiremock::{Mock, MockServer, ResponseTemplate, matchers::path};

        let server = MockServer::start().await;
        Mock::given(path("/gemini-error"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "7")
                    .insert_header("x-ratelimit-limit-requests", "120")
                    .insert_header("x-ratelimit-remaining-requests", "3")
                    .set_body_json(serde_json::json!({"error": {"message": "quota exhausted"}})),
            )
            .mount(&server)
            .await;

        let response = reqwest::Client::new()
            .get(format!("{}/gemini-error", server.uri()))
            .send()
            .await
            .expect("offline Gemini response should be available");
        let error = handle_provider_http_error(response, "Gemini", &NO_RATE_LIMIT_HEADERS)
            .await
            .expect_err("Gemini 429 response should become an LLM error");

        let LLMError::RateLimit { metadata: Some(metadata) } = error else {
            panic!("expected Gemini rate-limit metadata");
        };
        assert_eq!(metadata.retry_after.as_deref(), Some("7"));
        assert_eq!(metadata.rate_limit, None);
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

        let error = parse_api_error(ProviderHttpError::new(
            ProviderFailure::new(
                reqwest::StatusCode::TOO_MANY_REQUESTS,
                r#"{"error":{"message":"slow down"}}"#,
                &headers,
            ),
            "Baseten",
            &RateLimitHeaderConfig::default(),
        ));
        let LLMError::RateLimit { metadata: Some(metadata) } = error else {
            panic!("expected rate-limit metadata");
        };
        assert_eq!(metadata.retry_after.as_deref(), Some("7"));
        let rate_limit = metadata.rate_limit.as_ref().expect("mapped rate-limit headers");
        let observed = [
            rate_limit.requests_limit_per_minute,
            rate_limit.requests_remaining_per_minute,
            rate_limit.tokens_limit_per_minute,
            rate_limit.tokens_remaining_per_minute,
        ];
        assert_eq!(observed, [Some(120), Some(3), Some(50_000), Some(400)]);
        let serialized = serde_json::to_string(&metadata).expect("metadata serialization");
        assert!(
            !serialized.contains("x-secret-provider-note") && !serialized.contains("do-not-retain"),
            "raw provider headers must not be retained: {serialized}"
        );
    }

    #[test]
    fn parse_moonshot_auth_error_includes_platform_key_guidance() {
        let error = parse_provider_error(
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
        let err = parse_provider_error(
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
        let err = parse_provider_error(
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
        let err = parse_provider_error(
            "Qwen",
            reqwest::StatusCode::UNAUTHORIZED,
            r#"{"error":{"message":"Invalid API key"}}"#,
        );
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
        let err = parse_provider_error(
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
