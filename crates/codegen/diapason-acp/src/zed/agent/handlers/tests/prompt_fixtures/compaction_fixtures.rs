//! Compaction fixtures for prompt-routing tests.
//!
//! One provider answers with a configured summary after a deterministic delay;
//! the other blocks once its compaction request starts, so cancellation and
//! elapsed-time assertions have explicit inputs.

use super::*;

pub(in crate::zed::agent::handlers) struct CompactThenAnswerProvider {
    pub(in crate::zed::agent::handlers) requests: Arc<Mutex<Vec<LLMRequest>>>,
    pub(in crate::zed::agent::handlers) context_size: usize,
    pub(in crate::zed::agent::handlers) summary: String,
    /// Deterministic stall before the compaction response.
    ///
    /// Elapsed-time metrics need a controlled positive interval: a zero-delay
    /// provider can finish inside the first millisecond and let an assertion
    /// pass even when duration recording is broken.
    pub(in crate::zed::agent::handlers) delay: Duration,
}

pub(in crate::zed::agent::handlers) struct PendingCompactionProvider {
    started: Arc<tokio::sync::Notify>,
}

impl PendingCompactionProvider {
    /// Create a provider that blocks until its compaction request starts.
    pub(in crate::zed::agent::handlers) fn new(started: Arc<tokio::sync::Notify>) -> Self {
        Self { started }
    }
}

#[async_trait]
impl LLMProvider for CompactThenAnswerProvider {
    /// Return the provider name used by prompt-routing tests.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Report the configured context size that triggers compaction.
    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
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

    /// Stream a canned answer after compaction.
    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.requests.lock().expect("compaction provider requests").push(request);
        let response = LLMResponse {
            content: Some("Answer after compaction.".to_string()),
            finish_reason: diapason_core::llm::provider::FinishReason::Stop,
            model: "wire-model".to_string(),
            ..LLMResponse::default()
        };
        Ok(Box::pin(futures::stream::iter(vec![
            Ok(LLMStreamEvent::Token { delta: "Answer after compaction.".to_string() }),
            Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
        ])))
    }

    /// Return the configured compaction summary and record the request.
    ///
    /// A configured delay keeps elapsed-time assertions deterministic.
    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("compaction provider requests").push(request);
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        Ok(LLMResponse::new("wire-model", self.summary.clone()))
    }
}

#[async_trait]
impl LLMProvider for PendingCompactionProvider {
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

    /// Block after signalling that the compaction request started.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.started.notify_one();
        std::future::pending().await
    }
}
