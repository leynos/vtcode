//! Prompt providers whose first or only attempt fails in a specific way.

use super::*;

/// Fails the compaction summary request after signalling that it started, so
/// a transport test can observe the terminal failure update.
pub(in crate::zed::agent::handlers) struct FailingCompactionProvider {
    started: Arc<tokio::sync::Notify>,
}

impl FailingCompactionProvider {
    /// Create a provider that fails after signalling its compaction request.
    pub(in crate::zed::agent::handlers) fn new(started: Arc<tokio::sync::Notify>) -> Self {
        Self { started }
    }
}

pub(in crate::zed::agent::handlers) struct PartialThenFailProvider;

/// Emits one token and then a provider-supplied 429, so the failure is
/// terminal: partial output is already visible, and the retry budget cannot
/// resume a stream the user has seen.
pub(in crate::zed::agent::handlers) struct PartialThenRateLimitProvider;

/// Fails the first stream attempt with a retryable 429 before any output is
/// visible, then answers on the next attempt. A transport test can therefore
/// observe the rate-limit notice and the scheduled retry it announces.
pub(in crate::zed::agent::handlers) struct RetryableRateLimitThenAnswerProvider {
    calls: AtomicUsize,
}

impl RetryableRateLimitThenAnswerProvider {
    /// Create a provider that rate-limits once before answering.
    pub(in crate::zed::agent::handlers) fn new() -> Self {
        Self { calls: AtomicUsize::new(0) }
    }
}

#[async_trait]
impl LLMProvider for FailingCompactionProvider {
    /// Return the provider name used by prompt-routing tests.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Report a small context size so compaction is attempted.
    fn effective_context_size(&self, _model: &str) -> usize {
        8_192
    }

    /// Return the models this fixture serves.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Accept every request.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Fail after signalling that the compaction request started.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.started.notify_one();
        Err(LLMError::Provider {
            message: "compaction summary request failed".to_string(),
            metadata: None,
        })
    }
}

#[async_trait]
impl LLMProvider for PartialThenFailProvider {
    /// Return the provider name used by prompt-routing tests.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Return the models this fixture serves.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Report that this fixture streams responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Accept every request.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Emit one token and then a terminal network failure.
    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "partial answer".to_string() }),
            Err(LLMError::Network {
                message: "fixture stream disconnected".to_string(),
                metadata: None,
            }),
        ])))
    }

    /// Panic, because this fixture is only used for streaming.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("partial-stream test must not use buffered generation")
    }
}

#[async_trait]
impl LLMProvider for PartialThenRateLimitProvider {
    /// Return the provider name used by prompt-routing tests.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Return the models this fixture serves.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Report that this fixture streams responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Accept every request.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Emit one token and then a provider rate-limit error.
    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "partial answer".to_string() }),
            Err(LLMError::RateLimit {
                metadata: Some(LLMErrorMetadata::new(
                    "wire-test",
                    Some(429),
                    Some("rate_limit_error".to_string()),
                    None,
                    None,
                    Some("12".to_string()),
                    None,
                )),
            }),
        ])))
    }

    /// Panic, because this fixture is only used for streaming.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("rate-limited stream test must not use buffered generation")
    }
}

#[async_trait]
impl LLMProvider for RetryableRateLimitThenAnswerProvider {
    /// Return the provider name used by prompt-routing tests.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Return the models this fixture serves.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Report that this fixture streams responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Accept every request.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Fail the first attempt with a retryable 429, then emit the answer.
    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(LLMError::RateLimit {
                metadata: Some(LLMErrorMetadata::new(
                    "wire-test",
                    Some(429),
                    Some("rate_limit_error".to_string()),
                    None,
                    None,
                    Some("0.5".to_string()),
                    None,
                )),
            });
        }
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "retried answer".to_string() }),
            Ok(LLMStreamEvent::Completed {
                response: Box::new(LLMResponse {
                    content: Some("retried answer".to_string()),
                    finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                    model: "wire-model".to_string(),
                    ..LLMResponse::default()
                }),
            }),
        ])))
    }

    /// Panic, because this fixture is only used for streaming.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("scheduled-retry stream test must not use buffered generation")
    }
}
