//! Behavioural coverage for the Responses stream telemetry boundary.
//!
//! The tests inject a recording sink so every assertion is deterministic and
//! independent of the process-wide production counters. They pin
//! first-terminal-wins recording, error classification, abandonment on drop
//! (including an unpolled stream), and the monotonic WebSocket counters.

use std::sync::Mutex;

use super::{
    ResponsesObservation, ResponsesOutcomeRecorder, ResponsesStreamOutcome, ResponsesStreamPath,
    ResponsesTelemetrySink, observe_responses_stream, record_responses_replay_prohibited,
    record_responses_websocket_fallback, responses_websocket_metrics,
};
use crate::provider::LLMError;
use futures::StreamExt;
use futures::stream;

/// Recording sink that keeps every observation for assertions.
#[derive(Debug, Default)]
struct TestSink {
    observations: Mutex<Vec<ResponsesObservation>>,
}

impl ResponsesTelemetrySink for TestSink {
    fn record(&self, observation: &ResponsesObservation) {
        self.observations.lock().expect("test sink lock").push(observation.clone());
    }
}

impl TestSink {
    fn observations(&self) -> Vec<ResponsesObservation> {
        self.observations.lock().expect("test sink lock").clone()
    }
}

/// Build a provider error carrying sentinel text that must never be recorded.
fn provider_error() -> LLMError {
    LLMError::Provider {
        message: "sensitive provider detail".to_string(),
        metadata: None,
    }
}

/// Prove only the first terminal outcome for an attempt is recorded.
#[test]
fn recorder_keeps_only_the_first_terminal_outcome() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Shared, "TestProvider", sink.clone());

    assert!(recorder.record(ResponsesStreamOutcome::Incomplete));
    assert!(!recorder.record(ResponsesStreamOutcome::Abandoned));

    let observations = sink.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, ResponsesStreamOutcome::Incomplete);
    assert_eq!(observations[0].path, ResponsesStreamPath::Shared);
}

/// Prove an early error is recorded once without provider payload content.
#[tokio::test]
async fn early_error_is_recorded_once_without_payload_content() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Legacy, "TestProvider", sink.clone());
    let inner = stream::iter(vec![Ok(1_u8), Err(provider_error())]);
    let observed = observe_responses_stream(inner, std::sync::Arc::clone(&recorder));

    let items: Vec<_> = observed.collect().await;

    assert_eq!(items.len(), 2);
    assert!(items[1].is_err());
    let observations = sink.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, ResponsesStreamOutcome::ProviderFailure);
    let serialized = format!("{observations:?}");
    assert!(!serialized.contains("sensitive provider detail"));
}

/// Prove a network error is classified as a transport failure.
#[tokio::test]
async fn transport_error_is_classified_as_transport_failure() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Shared, "TestProvider", sink.clone());
    let inner = stream::iter(vec![Err::<u8, _>(LLMError::Network {
        message: "socket closed".to_string(),
        metadata: None,
    })]);

    let items: Vec<_> = observe_responses_stream(inner, recorder).collect().await;

    assert_eq!(items.len(), 1);
    assert_eq!(sink.observations()[0].outcome, ResponsesStreamOutcome::TransportFailure);
}

/// Prove dropping a partially consumed stream records one abandonment.
#[tokio::test]
async fn dropping_a_stream_before_a_terminal_records_abandonment() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Legacy, "TestProvider", sink.clone());
    let inner = stream::iter(vec![Ok::<u8, LLMError>(1)]);
    let mut observed = Box::pin(observe_responses_stream(inner, recorder));

    assert!(matches!(observed.next().await, Some(Ok(1))));
    drop(observed);

    let observations = sink.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, ResponsesStreamOutcome::Abandoned);
}

/// Prove dropping an unpolled stream still records one abandonment.
#[tokio::test]
async fn dropping_an_unpolled_stream_records_abandonment() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Shared, "TestProvider", sink.clone());
    let inner = stream::iter(vec![Ok::<u8, LLMError>(1)]);

    let observed = observe_responses_stream(inner, recorder);
    drop(observed);

    let observations = sink.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, ResponsesStreamOutcome::Abandoned);
}

/// Prove a recorded terminal outcome is not overwritten by the drop guard.
#[tokio::test]
async fn a_recorded_terminal_outcome_is_not_overwritten_by_drop() {
    let sink = std::sync::Arc::new(TestSink::default());
    let recorder = ResponsesOutcomeRecorder::new(ResponsesStreamPath::Shared, "TestProvider", sink.clone());
    let inner = stream::iter(vec![Ok::<u8, LLMError>(1)]);
    let mut observed = Box::pin(observe_responses_stream(inner, std::sync::Arc::clone(&recorder)));

    assert!(matches!(observed.next().await, Some(Ok(1))));
    assert!(recorder.record(ResponsesStreamOutcome::Completed));
    drop(observed);

    let observations = sink.observations();
    assert_eq!(observations.len(), 1);
    assert_eq!(observations[0].outcome, ResponsesStreamOutcome::Completed);
}

/// Prove the process-wide WebSocket counters only move forward.
#[test]
fn websocket_recovery_counters_are_monotonic() {
    let before = responses_websocket_metrics();
    record_responses_replay_prohibited();
    record_responses_websocket_fallback();
    let after = responses_websocket_metrics();

    assert!(
        after.replay_prohibited_total > before.replay_prohibited_total,
        "replay prohibition must increment replay_prohibited_total"
    );
    assert!(
        after.http_fallback_total > before.http_fallback_total,
        "fallback must increment http_fallback_total"
    );
}
