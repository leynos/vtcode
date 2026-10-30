//! Real automatic-orchestration contracts for cached prefire and private metrics.

use super::*;
use crate::compaction::{AsyncCompactionCache, PrefireState, SUPPRESS_NONE};
use crate::llm::provider::{LLMError, LLMResponse, MessageRole, ResponsesCompactionOptions};
use crate::telemetry::TelemetryEvent;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

mod metrics_tests;

const MODEL: &str = "MODEL_PAYLOAD://private-model";
const SESSION: &str = "SESSION_PAYLOAD-private-session";
const PROMPT: &str = "PROMPT_PAYLOAD preserve the original task";

struct RecordingProvider {
    requests: Mutex<Vec<LLMRequest>>,
    context_size: usize,
    failure: Option<String>,
    summary: String,
    native_compaction: bool,
    native_requests: AtomicUsize,
    capability_queries: AtomicUsize,
}

#[async_trait]
impl LLMProvider for RecordingProvider {
    fn name(&self) -> &str {
        "PROVIDER_PAYLOAD-private-provider"
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("request recorder lock").push(request);
        match &self.failure {
            Some(message) => Err(LLMError::Provider { message: message.clone(), metadata: None }),
            None => Ok(LLMResponse::new(MODEL, self.summary.clone())),
        }
    }

    fn supported_models(&self) -> Vec<String> {
        vec![MODEL.to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        self.context_size
    }

    fn supports_manual_openai_compaction(&self, _model: &str) -> bool {
        self.capability_queries.fetch_add(1, Ordering::Relaxed);
        self.native_compaction
    }

    async fn compact_history_with_options(
        &self,
        _model: &str,
        _history: &[Message],
        _options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.native_requests.fetch_add(1, Ordering::Relaxed);
        Err(LLMError::Provider {
            message: "ERROR_PAYLOAD native failure".to_string(),
            metadata: None,
        })
    }
}

struct AutoScenario {
    workspace: tempfile::TempDir,
    config: VTCodeConfig,
    provider: RecordingProvider,
    prefire: PrefireState,
    history: Vec<Message>,
    suppressed: u8,
    engine: CompactionConfig,
    metrics: Arc<Mutex<Vec<TelemetryEvent>>>,
}

impl AutoScenario {
    fn new(context_size: usize) -> Self {
        let mut config = VTCodeConfig::default();
        config.agent.harness.auto_compaction_enabled = true;
        config.agent.harness.auto_compaction_threshold_tokens = Some(1);
        config.context.dynamic.enabled = false;
        config.context.dynamic.persist_history = false;
        Self {
            workspace: tempfile::tempdir().expect("scenario workspace"),
            config,
            provider: RecordingProvider {
                requests: Mutex::new(Vec::new()),
                context_size,
                failure: None,
                summary: "verified compact summary".to_string(),
                native_compaction: false,
                native_requests: AtomicUsize::new(0),
                capability_queries: AtomicUsize::new(0),
            },
            prefire: PrefireState::default(),
            history: vec![
                Message::user(PROMPT.to_string()),
                Message::assistant("completed tool observations".to_string()),
                Message::user("latest steering instructions".to_string()),
            ],
            suppressed: SUPPRESS_NONE,
            engine: CompactionConfig {
                keep_last_messages: 0,
                ..CompactionConfig::default()
            },
            metrics: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn cache(&self, note: String) -> AsyncCompactionCache {
        AsyncCompactionCache {
            note1: note,
            prefix_len: 2,
            fingerprint: fingerprint_prefix(&self.history[..2]),
            model_slug: MODEL.to_string(),
            pass1_latency_ms: 17,
        }
    }

    async fn run(&mut self, force: bool) -> Result<Option<AutoCompactionOutcome>> {
        let sink = AutoCompactionMetricSink::Test(Arc::clone(&self.metrics));
        self.execute(force, Some(&sink)).await
    }

    async fn run_public(&mut self, force: bool) -> Result<Option<AutoCompactionOutcome>> {
        self.execute(force, None).await
    }

    async fn execute(
        &mut self,
        force: bool,
        metrics: Option<&AutoCompactionMetricSink>,
    ) -> Result<Option<AutoCompactionOutcome>> {
        let input = AutoCompactionInput {
            provider: &self.provider,
            model: MODEL,
            session_id: SESSION,
            workspace_root: self.workspace.path(),
            vt_cfg: Some(&self.config),
            current_token_usage: self.history.iter().map(Message::estimate_tokens).sum(),
            touched_files: &[],
            engine_cfg: self.engine.clone(),
            manual_options: ManualCompactionOptions::default(),
            placement: MemoryEnvelopePlacement::BeforeLastUserOrSummary,
            prefire: Some(&self.prefire),
            auto_compact_suppressed: &mut self.suppressed,
            force_compaction: force,
            steering_update: None,
        };
        match metrics {
            Some(sink) => auto_compact_messages_with_metrics(input, &mut self.history, sink).await,
            None => auto_compact_messages(input, &mut self.history).await,
        }
    }

    fn recorded_metrics(&self) -> Vec<TelemetryEvent> {
        self.metrics.lock().expect("metric recorder lock").clone()
    }
}

/// Counter contracts are independent of duration values and terminal labels.
fn assert_metric_counters(events: &[TelemetryEvent], expected_counter: &str) {
    assert_eq!(events[0].name, "vtcode.compaction.auto_attempt_total");
    assert_eq!(events.iter().filter(|event| event.name == expected_counter).count(), 1);
    for event in events.iter().filter(|event| event.name != "vtcode.compaction.auto_duration_ms") {
        assert_eq!(event.value.to_bits(), 1.0_f64.to_bits());
    }
}

/// The public metric vocabulary never grows from arbitrary runtime inputs.
fn assert_metric_vocabulary(events: &[TelemetryEvent]) {
    let names = [
        "vtcode.compaction.auto_attempt_total",
        "vtcode.compaction.auto_success_total",
        "vtcode.compaction.auto_no_change_total",
        "vtcode.compaction.auto_failure_total",
        "vtcode.compaction.auto_protected_budget_refusal_total",
        "vtcode.compaction.auto_duration_ms",
    ];
    for event in events {
        assert!(names.contains(&event.name.as_str()));
        assert_eq!(event.tags.len(), 3);
        assert!(
            event
                .tags
                .keys()
                .all(|key| matches!(key.as_str(), "mode" | "outcome" | "error_category"))
        );
    }
}

/// An eligible terminal result carries exactly one finite non-negative duration.
fn assert_metric_duration(events: &[TelemetryEvent]) -> &TelemetryEvent {
    let durations = events
        .iter()
        .filter(|event| event.name == "vtcode.compaction.auto_duration_ms")
        .collect::<Vec<_>>();
    assert_eq!(durations.len(), 1);
    assert!(durations[0].value.is_finite());
    assert!(durations[0].value >= 0.0);
    durations[0]
}

fn assert_terminal_labels(event: &TelemetryEvent, expected_outcome: &str, expected_category: &str) {
    assert_eq!(event.tags.get("outcome").map(String::as_str), Some(expected_outcome));
    assert_eq!(event.tags.get("error_category").map(String::as_str), Some(expected_category));
}

fn terminal_counter_name(outcome: &str) -> &'static str {
    match outcome {
        "success" => "vtcode.compaction.auto_success_total",
        "no_change" => "vtcode.compaction.auto_no_change_total",
        "failure" => "vtcode.compaction.auto_failure_total",
        other => panic!("unsupported terminal outcome: {other}"),
    }
}

/// Combine the independently named counter, vocabulary, and timing contracts.
fn assert_terminal_metrics(events: &[TelemetryEvent], expected_outcome: &str, expected_category: &str) {
    assert_metric_counters(events, terminal_counter_name(expected_outcome));
    assert_metric_vocabulary(events);
    let duration = assert_metric_duration(events);
    assert_terminal_labels(duration, expected_outcome, expected_category);
    assert_eq!(events[0].tags.get("mode").map(String::as_str), Some("unknown"));
}

/// Cache identity binds the note to the same prefix and model.
fn assert_cache_identity(actual: &AsyncCompactionCache, expected: &AsyncCompactionCache) {
    assert_eq!(actual.prefix_len, expected.prefix_len);
    assert_eq!(actual.fingerprint, expected.fingerprint);
    assert_eq!(actual.model_slug, expected.model_slug);
}

fn assert_no_provider_dispatch(provider: &RecordingProvider) {
    assert!(provider.requests.lock().expect("requests").is_empty());
    assert_eq!(provider.native_requests.load(Ordering::Relaxed), 0);
    assert_eq!(provider.capability_queries.load(Ordering::Relaxed), 0);
}

fn assert_task_and_steering_retained(history: &[Message], original: &[Message]) {
    assert!(history.contains(&original[0]));
    assert!(history.contains(&original[2]));
}

/// A fallback makes one ordinary summary request with no cached-note carrier.
fn assert_single_pass_request(requests: &[LLMRequest], cache_marker: &str) {
    assert_eq!(requests.len(), 1, "no pass-two request may precede fallback");
    assert!(
        !requests[0]
            .messages
            .iter()
            .any(|message| message.content.as_text().contains(cache_marker))
    );
    assert!(
        requests[0]
            .messages
            .last()
            .expect("summary prompt")
            .content
            .as_text()
            .contains("Summarize")
    );
}

fn assert_cached_request(requests: &[LLMRequest], cache_marker: &str) {
    assert_eq!(requests.len(), 1, "valid prefire must not fall back to another summary request");
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|message| message.content.as_text().contains(cache_marker))
    );
}

#[tokio::test]
async fn populated_prefire_refuses_oversized_task_before_cache_take_or_provider_request() {
    let mut scenario = AutoScenario::new(2_048);
    scenario.history[0] = Message::user(PROMPT.repeat(1_000));
    let original = scenario.history.clone();
    let cache = scenario.cache("cached note survives refusal".to_string());
    scenario.prefire.store(cache.clone());

    let error = scenario.run_public(true).await.expect_err("protected task must not be clipped");

    assert!(crate::compaction::is_protected_user_budget_error(&error));
    assert_eq!(scenario.history, original);
    assert_no_provider_dispatch(&scenario.provider);
    assert!(scenario.prefire.has_cache());
    let retained = scenario.prefire.take().expect("cache remains available");
    assert_eq!(retained.note1, cache.note1);
    assert_cache_identity(&retained, &cache);
    assert_eq!(retained.pass1_latency_ms, cache.pass1_latency_ms);
}

#[tokio::test]
async fn oversized_prefire_pass_two_falls_back_to_one_single_pass_request() {
    let mut scenario = AutoScenario::new(2_048);
    let original = scenario.history.clone();
    let cache_marker = "cached-note-private-marker";
    scenario.prefire.store(scenario.cache(cache_marker.repeat(1_000)));

    let outcome = scenario
        .run_public(true)
        .await
        .expect("single-pass fallback succeeds")
        .expect("compaction ran");

    let requests = scenario.provider.requests.lock().expect("requests");
    assert_single_pass_request(&requests, cache_marker);
    assert!(!scenario.prefire.has_cache());
    assert_ne!(scenario.history, original);
    assert_task_and_steering_retained(&scenario.history, &original);
    assert_eq!(outcome.mode, CompactionMode::Local);
}

#[tokio::test]
async fn successful_populated_prefire_preserves_prompts_and_final_context_bound() {
    let mut scenario = AutoScenario::new(8_192);
    let original = scenario.history.clone();
    let cache_marker = "prefire note used for pass two";
    scenario.prefire.store(scenario.cache(cache_marker.to_string()));

    let outcome = scenario.run(true).await.expect("prefire succeeds").expect("compaction ran");

    let requests = scenario.provider.requests.lock().expect("requests");
    assert_cached_request(&requests, cache_marker);
    assert_task_and_steering_retained(&scenario.history, &original);
    assert!(scenario.history.iter().map(Message::estimate_tokens).sum::<usize>() <= 6_656);
    assert_eq!(outcome.mode, CompactionMode::Local);
    let metrics = scenario.recorded_metrics();
    assert_eq!(metrics.len(), 3);
    assert_terminal_metrics(&metrics, "success", "none");
    assert_eq!(scenario.provider.capability_queries.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn successful_prefire_bounds_oversized_summary_before_memory_envelope_injection() {
    let mut scenario = AutoScenario::new(2_048);
    scenario.history[1] = Message::assistant("completed investigation observations ".repeat(1_000));
    scenario.provider.summary = "detailed completed investigation summary ".repeat(1_000);
    let original = scenario.history.clone();
    let cache_marker = "valid cached prefix note";
    scenario.prefire.store(scenario.cache(cache_marker.to_string()));
    let budget = crate::compaction::compaction_history_budget(&scenario.provider, MODEL).expect("known context");
    assert!(Message::system(scenario.provider.summary.clone()).estimate_tokens() > budget);

    let compacted =
        try_two_pass_with_prefire(&scenario.prefire, &scenario.provider, MODEL, &scenario.history, &scenario.engine)
            .await
            .expect("valid prefire succeeds")
            .expect("cached path produces bounded history");

    let users = compacted
        .iter()
        .filter(|message| message.role == MessageRole::User)
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(users, vec![original[0].clone(), original[2].clone()]);
    assert!(compacted.iter().map(Message::estimate_tokens).sum::<usize>() <= budget);
    let requests = scenario.provider.requests.lock().expect("requests");
    assert_cached_request(&requests, cache_marker);
    assert!(!scenario.prefire.has_cache());
    assert_eq!(scenario.history, original, "candidate finalization must not publish live history");
}
