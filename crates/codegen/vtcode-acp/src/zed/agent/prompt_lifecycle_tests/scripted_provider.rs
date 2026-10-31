//! Scripted `LLMProvider` used by the prompt lifecycle transport tests.

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use vtcode_core::llm::provider::{
    FinishReason, LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent,
};

pub(super) const MODEL: &str = "lifecycle-model";

/// One scripted buffered provider response.
#[derive(Clone, Copy)]
pub(super) enum ScriptedStep {
    Answer(&'static str),
    Empty,
    Failure,
}

/// Shared scripted-provider state, `Arc`-backed so the agent and the test can
/// observe and release each generation.
struct ScriptedShared {
    steps: Mutex<VecDeque<ScriptedStep>>,
    call_started: Notify,
    call_release: Notify,
    /// When set, the next `generate` waits for `call_release` before returning.
    hold_next: AtomicBool,
    calls: AtomicUsize,
}

#[derive(Clone)]
pub(super) struct ScriptedProvider {
    name: String,
    shared: Arc<ScriptedShared>,
    /// When set, turns call `stream` instead of `generate`.
    streaming: bool,
}

impl ScriptedProvider {
    pub(super) fn new(name: &str, steps: Vec<ScriptedStep>, hold_first: bool, streaming: bool) -> Self {
        Self {
            name: name.to_string(),
            shared: Arc::new(ScriptedShared {
                steps: Mutex::new(VecDeque::from(steps)),
                call_started: Notify::new(),
                call_release: Notify::new(),
                hold_next: AtomicBool::new(hold_first),
                calls: AtomicUsize::new(0),
            }),
            streaming,
        }
    }

    fn take_step(&self) -> ScriptedStep {
        // Pop before waiting so a cancelled generation still consumes its
        // scripted step and cannot shift the next turn's response.
        self.shared
            .steps
            .lock()
            .expect("scripted lifecycle steps")
            .pop_front()
            .unwrap_or(ScriptedStep::Empty)
    }

    /// Counts a provider entry, signals the test, and applies the optional hold.
    async fn begin_call(&self, step: ScriptedStep) -> ScriptedStep {
        let _previous_calls = self.shared.calls.fetch_add(1, Ordering::SeqCst);
        self.shared.call_started.notify_one();
        if self.shared.hold_next.swap(false, Ordering::SeqCst) {
            self.shared.call_release.notified().await;
        }
        step
    }

    pub(super) fn call_started(&self) -> &Notify {
        &self.shared.call_started
    }

    pub(super) fn release_call(&self) {
        self.shared.call_release.notify_one();
    }

    pub(super) fn call_count(&self) -> usize {
        self.shared.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supported_models(&self) -> Vec<String> {
        vec![MODEL.to_string()]
    }

    fn supports_streaming(&self) -> bool {
        self.streaming
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let step = self.begin_call(self.take_step()).await;
        match step {
            ScriptedStep::Answer(text) => Ok(LLMResponse {
                content: Some(text.to_string()),
                finish_reason: FinishReason::Stop,
                model: MODEL.to_string(),
                ..LLMResponse::default()
            }),
            ScriptedStep::Empty => Ok(LLMResponse {
                content: None,
                finish_reason: FinishReason::Stop,
                model: MODEL.to_string(),
                ..LLMResponse::default()
            }),
            ScriptedStep::Failure => Err(LLMError::InvalidRequest {
                message: "scripted lifecycle provider failure".to_string(),
                metadata: None,
            }),
        }
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        if !self.streaming {
            return Err(LLMError::InvalidRequest {
                message: "lifecycle buffered harness does not stream".to_string(),
                metadata: None,
            });
        }
        let step = self.begin_call(self.take_step()).await;
        match step {
            ScriptedStep::Answer(text) => {
                let response = LLMResponse {
                    content: Some(text.to_string()),
                    finish_reason: FinishReason::Stop,
                    model: MODEL.to_string(),
                    ..LLMResponse::default()
                };
                Ok(Box::pin(futures::stream::iter([Ok(LLMStreamEvent::Completed { response: Box::new(response) })])))
            }
            // An empty stream ends before the completion event, which is the
            // truncated-stream shape the retry policy must classify.
            ScriptedStep::Empty => Ok(Box::pin(futures::stream::iter(Vec::new()))),
            ScriptedStep::Failure => Err(LLMError::InvalidRequest {
                message: "scripted lifecycle provider failure".to_string(),
                metadata: None,
            }),
        }
    }
}
