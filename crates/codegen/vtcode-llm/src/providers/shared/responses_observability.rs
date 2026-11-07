//! Bounded observability for Responses stream terminal outcomes.
//!
//! The provider layer records one terminal outcome per stream attempt. Every
//! label is a finite enum variant, so observations never carry response text,
//! tool input, request identifiers, or provider diagnostics. Production
//! records land in process-wide counters that the core runtime can sample;
//! tests inject their own sink, which keeps recording deterministic and free
//! of global test hooks.

use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use async_stream::stream;
use futures::{Stream, StreamExt, pin_mut};
use vtcode_commons::ErrorCategory;

use crate::provider::LLMError;

/// Which Responses streaming implementation produced the observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponsesStreamPath {
    /// The shared normalized stream used by Responses-capable providers.
    Shared,
    /// The legacy OpenAI `LLMStreamEvent` decoder retained for compatibility.
    Legacy,
}

impl ResponsesStreamPath {
    const COUNT: usize = 2;

    const fn index(self) -> usize {
        match self {
            Self::Shared => 0,
            Self::Legacy => 1,
        }
    }

    /// Stable, bounded metric label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shared => "shared",
            Self::Legacy => "legacy",
        }
    }
}

/// How one Responses stream attempt terminated.
///
/// The vocabulary is closed: a new failure mode needs a new variant and a
/// deliberate decision about its label, so cardinality cannot drift with
/// provider payload content.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResponsesStreamOutcome {
    /// The stream produced a complete response.
    Completed,
    /// The stream ended without the provider's completion terminal.
    Incomplete,
    /// The completion terminal carried no response payload.
    MissingFinalResponse,
    /// A frame could not be decoded or reconciled as a valid event.
    MalformedEvent,
    /// Replaying the provider's completed response failed reconciliation.
    ReconciliationFailed,
    /// The completed response could not be parsed.
    FinalResponseInvalid,
    /// Usage or identifier metadata in the completed response was invalid.
    MetadataInvalid,
    /// Streamed tool calls could not be paired with the completed response.
    ToolCallReconciliationFailed,
    /// The provider or transport reported a failure frame.
    ProviderFailure,
    /// The response body failed while being read.
    TransportFailure,
    /// The consumer dropped the stream before any terminal outcome.
    Abandoned,
}

impl ResponsesStreamOutcome {
    const COUNT: usize = 11;

    const fn index(self) -> usize {
        match self {
            Self::Completed => 0,
            Self::Incomplete => 1,
            Self::MissingFinalResponse => 2,
            Self::MalformedEvent => 3,
            Self::ReconciliationFailed => 4,
            Self::FinalResponseInvalid => 5,
            Self::MetadataInvalid => 6,
            Self::ToolCallReconciliationFailed => 7,
            Self::ProviderFailure => 8,
            Self::TransportFailure => 9,
            Self::Abandoned => 10,
        }
    }

    /// Stable, bounded metric label.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Incomplete => "incomplete",
            Self::MissingFinalResponse => "missing_final_response",
            Self::MalformedEvent => "malformed_event",
            Self::ReconciliationFailed => "reconciliation_failed",
            Self::FinalResponseInvalid => "final_response_invalid",
            Self::MetadataInvalid => "metadata_invalid",
            Self::ToolCallReconciliationFailed => "tool_call_reconciliation_failed",
            Self::ProviderFailure => "provider_failure",
            Self::TransportFailure => "transport_failure",
            Self::Abandoned => "abandoned",
        }
    }

    /// Coarse tracing outcome: successful, rejected, or abandoned.
    const fn log_outcome(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
            _ => "rejected",
        }
    }

    /// Fixed diagnostic code preserved from the earlier tracing-only records.
    const fn error_category(self) -> &'static str {
        match self {
            Self::Completed => "responses_stream_completed",
            Self::Incomplete => "responses_stream_incomplete",
            Self::MissingFinalResponse => "responses_stream_missing_final_response",
            Self::MalformedEvent => "responses_stream_malformed_event",
            Self::ReconciliationFailed => "responses_reconciliation_failed",
            Self::FinalResponseInvalid => "responses_final_response_invalid",
            Self::MetadataInvalid => "responses_metadata_invalid",
            Self::ToolCallReconciliationFailed => "responses_tool_call_reconciliation_failed",
            Self::ProviderFailure => "responses_stream_provider_failure",
            Self::TransportFailure => "responses_stream_transport_error",
            Self::Abandoned => "responses_stream_abandoned",
        }
    }
}

/// One bounded terminal observation for a Responses stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResponsesObservation {
    /// Implementation that produced the observation.
    pub path: ResponsesStreamPath,
    /// Provider name as reported by the provider layer.
    pub provider: String,
    /// Finite terminal outcome.
    pub outcome: ResponsesStreamOutcome,
}

/// Output port for Responses observations.
///
/// Implementations must remain bounded and must not include response content,
/// tool arguments, or provider diagnostics in any exported record.
pub trait ResponsesTelemetrySink: Send + Sync {
    /// Record one terminal observation.
    fn record(&self, observation: &ResponsesObservation);
}

/// Process-wide production sink: counters plus one tracing record.
#[derive(Debug, Default)]
pub struct ProcessResponsesTelemetry;

impl ResponsesTelemetrySink for ProcessResponsesTelemetry {
    fn record(&self, observation: &ResponsesObservation) {
        stream_counters().increment(observation.path, observation.outcome);
        tracing::debug!(
            target = "vtcode::llm::responses::stream",
            provider = observation.provider,
            path = observation.path.as_str(),
            outcome = observation.outcome.log_outcome(),
            error_category = observation.outcome.error_category(),
            "Responses stream terminal outcome"
        );
    }
}

/// The process-wide production sink.
pub fn process_responses_telemetry() -> Arc<dyn ResponsesTelemetrySink> {
    static SINK: LazyLock<Arc<ProcessResponsesTelemetry>> = LazyLock::new(|| Arc::new(ProcessResponsesTelemetry));
    SINK.clone()
}

/// Record one terminal outcome at most once per stream attempt.
///
/// Early failures and the terminal path can both observe the same attempt; the
/// first observation wins, so a rejected stream is never counted twice.
pub(crate) struct ResponsesOutcomeRecorder {
    path: ResponsesStreamPath,
    provider: String,
    sink: Arc<dyn ResponsesTelemetrySink>,
    recorded: AtomicBool,
}

impl ResponsesOutcomeRecorder {
    pub(crate) fn new(
        path: ResponsesStreamPath,
        provider: impl Into<String>,
        sink: Arc<dyn ResponsesTelemetrySink>,
    ) -> Arc<Self> {
        Arc::new(Self {
            path,
            provider: provider.into(),
            sink,
            recorded: AtomicBool::new(false),
        })
    }

    /// Record `outcome` unless this attempt already recorded a terminal one.
    ///
    /// Returns whether this call recorded the terminal outcome.
    pub(crate) fn record(&self, outcome: ResponsesStreamOutcome) -> bool {
        if self.recorded.swap(true, Ordering::AcqRel) {
            return false;
        }
        self.sink.record(&ResponsesObservation {
            path: self.path,
            provider: self.provider.clone(),
            outcome,
        });
        true
    }

    fn is_recorded(&self) -> bool {
        self.recorded.load(Ordering::Acquire)
    }
}

/// Classify an error that reached the stream boundary.
///
/// Paths that know a finer category record it before the error propagates; this
/// fallback keeps unclassified early failures attributable.
fn classify_stream_error(error: &LLMError) -> ResponsesStreamOutcome {
    match error {
        LLMError::Network { .. } => ResponsesStreamOutcome::TransportFailure,
        _ => ResponsesStreamOutcome::ProviderFailure,
    }
}

/// Record `Abandoned` when a stream is dropped without a terminal outcome.
struct ResponsesAbandonGuard {
    recorder: Arc<ResponsesOutcomeRecorder>,
}

impl Drop for ResponsesAbandonGuard {
    fn drop(&mut self) {
        if !self.recorder.is_recorded() {
            self.recorder.record(ResponsesStreamOutcome::Abandoned);
        }
    }
}

/// Observe a Responses stream without altering its items.
///
/// This is the single boundary that covers early failures in both streaming
/// implementations: every `Err` is classified once, and consuming code that
/// stops polling is recorded as abandonment rather than silence. The guard is
/// built before the generator so that dropping the stream without polling it
/// still records one `Abandoned` outcome.
pub(crate) fn observe_responses_stream<S, T>(
    inner: S,
    recorder: Arc<ResponsesOutcomeRecorder>,
) -> impl Stream<Item = Result<T, LLMError>> + Send
where
    S: Stream<Item = Result<T, LLMError>> + Send,
    T: Send,
{
    let abandon_guard = ResponsesAbandonGuard { recorder: Arc::clone(&recorder) };
    stream! {
        let _abandon_guard = abandon_guard;
        pin_mut!(inner);
        while let Some(item) = inner.next().await {
            if let Err(error) = &item {
                recorder.record(classify_stream_error(error));
            }
            yield item;
        }
    }
}

/// Snapshot of one path/outcome counter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResponsesStreamMetric {
    /// Streaming implementation.
    pub path: ResponsesStreamPath,
    /// Terminal outcome.
    pub outcome: ResponsesStreamOutcome,
    /// Number of attempts that terminated with this outcome.
    pub total: u64,
}

#[derive(Debug, Default)]
struct ResponsesStreamCounters {
    counts: [[AtomicU64; ResponsesStreamOutcome::COUNT]; ResponsesStreamPath::COUNT],
}

impl ResponsesStreamCounters {
    fn increment(&self, path: ResponsesStreamPath, outcome: ResponsesStreamOutcome) {
        self.counts[path.index()][outcome.index()].fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Vec<ResponsesStreamMetric> {
        let mut metrics = Vec::with_capacity(ResponsesStreamPath::COUNT * ResponsesStreamOutcome::COUNT);
        for path in [ResponsesStreamPath::Shared, ResponsesStreamPath::Legacy] {
            for outcome in ALL_OUTCOMES {
                metrics.push(ResponsesStreamMetric {
                    path,
                    outcome,
                    total: self.counts[path.index()][outcome.index()].load(Ordering::Relaxed),
                });
            }
        }
        metrics
    }
}

const ALL_OUTCOMES: [ResponsesStreamOutcome; ResponsesStreamOutcome::COUNT] = [
    ResponsesStreamOutcome::Completed,
    ResponsesStreamOutcome::Incomplete,
    ResponsesStreamOutcome::MissingFinalResponse,
    ResponsesStreamOutcome::MalformedEvent,
    ResponsesStreamOutcome::ReconciliationFailed,
    ResponsesStreamOutcome::FinalResponseInvalid,
    ResponsesStreamOutcome::MetadataInvalid,
    ResponsesStreamOutcome::ToolCallReconciliationFailed,
    ResponsesStreamOutcome::ProviderFailure,
    ResponsesStreamOutcome::TransportFailure,
    ResponsesStreamOutcome::Abandoned,
];

fn stream_counters() -> &'static ResponsesStreamCounters {
    static COUNTERS: LazyLock<ResponsesStreamCounters> = LazyLock::new(ResponsesStreamCounters::default);
    &COUNTERS
}

/// Snapshot every Responses stream terminal counter.
///
/// Counters are process-wide and monotonic; a consumer samples deltas.
#[must_use]
pub fn responses_stream_metrics() -> Vec<ResponsesStreamMetric> {
    stream_counters().snapshot()
}

/// Process-wide WebSocket recovery counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResponsesWebSocketMetric {
    /// Stream attempts that failed after output began, so replay was refused.
    pub replay_prohibited_total: u64,
    /// Stream attempts that fell back from WebSocket to HTTP.
    pub http_fallback_total: u64,
}

#[derive(Debug, Default)]
struct ResponsesWebSocketCounters {
    replay_prohibited_total: AtomicU64,
    http_fallback_total: AtomicU64,
}

fn websocket_counters() -> &'static ResponsesWebSocketCounters {
    static COUNTERS: LazyLock<ResponsesWebSocketCounters> = LazyLock::new(ResponsesWebSocketCounters::default);
    &COUNTERS
}

/// Snapshot the WebSocket recovery counters.
#[must_use]
pub fn responses_websocket_metrics() -> ResponsesWebSocketMetric {
    let counters = websocket_counters();
    ResponsesWebSocketMetric {
        replay_prohibited_total: counters.replay_prohibited_total.load(Ordering::Relaxed),
        http_fallback_total: counters.http_fallback_total.load(Ordering::Relaxed),
    }
}

/// Record that a WebSocket failure prohibited automatic replay.
pub(crate) fn record_responses_replay_prohibited() {
    websocket_counters().replay_prohibited_total.fetch_add(1, Ordering::Relaxed);
}

/// Record that a WebSocket exchange fell back to HTTP.
pub(crate) fn record_responses_websocket_fallback() {
    websocket_counters().http_fallback_total.fetch_add(1, Ordering::Relaxed);
}

/// Stable category label for a provider error.
pub(crate) fn responses_error_category(error: &LLMError) -> &'static str {
    ErrorCategory::from(error).as_str()
}

#[cfg(test)]
#[path = "responses_observability_tests.rs"]
mod tests;
