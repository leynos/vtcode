//! Recorder-owned checks of the runtime admission/publication boundary and its finite metric contract.

use super::{AdmissionCandidate, AdmissionMetricSink, AdmissionOutcome, admit_and_publish};
use hashbrown::HashMap;
use std::sync::Arc;
use std::time::Duration;
use vtcode_core::core::threads::{ThreadBootstrap, ThreadManager};
use vtcode_core::llm::provider::Message;
use vtcode_core::telemetry::TelemetryEvent;
use vtcode_core::telemetry::perf::TestPerfRecorder;

const PRIVATE_MARKER: &str = "private://arbitrary-provider/model/session/error/prompt-body";

struct AdmissionRun {
    result: anyhow::Result<usize>,
    original_history: Vec<Message>,
    candidate_history: Vec<Message>,
    current_history: Vec<Message>,
    events: Vec<TelemetryEvent>,
}

async fn run_admission(changed: bool, budget: Option<usize>) -> AdmissionRun {
    let original_history = vec![Message::user(format!("{PRIVATE_MARKER}: original task"))];
    let candidate_history = vec![Message::user(format!("{PRIVATE_MARKER}: protected candidate"))];
    let thread = ThreadManager::new().start_thread_with_identifier(
        PRIVATE_MARKER,
        ThreadBootstrap::new(None).with_messages(original_history.clone()),
    );
    let recorder = Arc::new(TestPerfRecorder::new());
    let sink = AdmissionMetricSink::Test(Arc::clone(&recorder));
    let candidate = AdmissionCandidate {
        thread: thread.clone(),
        history: candidate_history.clone(),
        original_history_tokens: original_history.iter().map(Message::estimate_tokens).sum(),
        prompt_tokens: 400,
        configured_threshold: Some(1),
        admission_budget: budget,
        changed,
    };
    let result = admit_and_publish(candidate, &sink).await;
    AdmissionRun {
        result,
        original_history,
        candidate_history,
        current_history: thread.messages(),
        events: recorder.drain().await,
    }
}

fn assert_metric_vocabulary(events: &[TelemetryEvent], outcome: &str, reason: &str) {
    assert_eq!(
        events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>(),
        vec![
            "vtcode.acp.compaction_admission_attempt_total",
            "vtcode.acp.compaction_admission_result_total",
            "vtcode.acp.compaction_admission_duration_ms",
        ],
    );
    assert!(events[0].tags.is_empty(), "the attempt metric has no labels");
    let expected_tags = HashMap::from([
        ("outcome".to_owned(), outcome.to_owned()),
        ("reason".to_owned(), reason.to_owned()),
    ]);
    for event in &events[1..] {
        assert_eq!(event.tags, expected_tags, "labels must stay within the closed decision vocabulary");
    }
    assert!(!format!("{events:?}").contains(PRIVATE_MARKER), "private request data must not enter telemetry");
}

fn assert_metric_values(events: &[TelemetryEvent]) {
    for event in &events[..2] {
        assert_eq!(event.value.to_bits(), 1.0_f64.to_bits(), "each boundary contributes exactly one counter");
    }
    assert!(events[2].value.is_finite());
    assert!(events[2].value >= 0.0, "admission duration must be nonnegative milliseconds");
}

#[tokio::test]
async fn over_budget_candidate_is_observed_without_replacing_live_history() {
    let run = run_admission(true, Some(1)).await;
    let error = run
        .result
        .as_ref()
        .expect_err("candidate must exceed admission budget")
        .to_string();
    assert!(error.starts_with("ACP compaction reduced the prompt from an estimated 400 to "));
    assert!(error.ends_with(" tokens, which still exceeds its safe provider admission budget"));
    assert_eq!(run.current_history, run.original_history, "refused history must remain unpublished");
    assert_metric_vocabulary(&run.events, "refused", "candidate_over_budget");
    assert_metric_values(&run.events);
}

#[tokio::test]
async fn unchanged_over_budget_prompt_is_a_distinct_refusal() {
    let run = run_admission(false, Some(1)).await;
    assert_eq!(
        run.result
            .as_ref()
            .expect_err("unchanged prompt must exceed admission budget")
            .to_string(),
        "ACP prompt uses an estimated 400 tokens, exceeds its safe provider admission budget, and automatic compaction made no change",
    );
    assert_eq!(run.current_history, run.original_history);
    assert_metric_vocabulary(&run.events, "refused", "unchanged_over_budget");
    assert_metric_values(&run.events);
}

#[tokio::test]
async fn accepted_candidate_is_observed_and_published() {
    let run = run_admission(true, Some(1_000)).await;
    assert!(run.result.is_ok());
    assert_eq!(run.current_history, run.candidate_history);
    assert_metric_vocabulary(&run.events, "accepted", "within_budget");
    assert_metric_values(&run.events);
}

#[tokio::test]
async fn unchanged_safe_prompt_is_observed_without_publishing_candidate() {
    let run = run_admission(false, Some(1_000)).await;
    assert_eq!(*run.result.as_ref().expect("safe unchanged prompt"), 400);
    assert_eq!(run.current_history, run.original_history);
    assert_metric_vocabulary(&run.events, "no_change", "unchanged_within_budget");
    assert_metric_values(&run.events);
}

#[tokio::test]
async fn admission_duration_uses_milliseconds() {
    let recorder = Arc::new(TestPerfRecorder::new());
    let sink = AdmissionMetricSink::Test(Arc::clone(&recorder));
    sink.record(AdmissionOutcome::Accepted, Duration::from_millis(37)).await;
    let events = recorder.drain().await;
    assert_metric_vocabulary(&events, "accepted", "within_budget");
    assert_eq!(events[2].value.to_bits(), 37.0_f64.to_bits());
}
