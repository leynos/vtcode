//! Behavioural coverage for the optional Responses WebSocket transport.
//!
//! The tests cover replay prohibition, URL derivation, continuation cache and
//! incremental-input policy, and scripted-server recovery paths. They drive the
//! same production helpers and provider type as the transport module.

use super::{
    OPENAI_BETA_RESPONSES_WEBSOCKET_V2, OUTPUT_STARTED_ERROR_CODE, OpenAIResponsesWebSocketContinuationCache,
    PREVIOUS_RESPONSE_NOT_FOUND_CODE, WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX, WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE,
    apply_generate_mode, input_is_incremental, is_websocket_active_response_error, is_websocket_connection_limit_error,
    is_websocket_previous_response_not_found_error, prepare_websocket_event, prohibit_websocket_replay,
    record_websocket_http_fallback, responses_output_started_error, responses_websocket_url,
    websocket_error_prohibits_replay,
};
use crate::provider::LLMError;
use crate::providers::shared::{
    ResponsesRecoveryObservation, ResponsesRecoveryTelemetry, process_responses_recovery_telemetry,
    responses_websocket_metrics,
};
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use std::fmt::Write as _;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

mod recovery;
mod scripted;
mod support;

/// Prove the pure conversion preserves the stable output-started code.
#[test]
fn output_started_conversion_preserves_the_output_started_code() {
    let converted = responses_output_started_error(LLMError::Provider {
        message: "sensitive provider detail".to_string(),
        metadata: None,
    });

    match converted {
        LLMError::Provider { metadata: Some(metadata), .. } => {
            assert_eq!(metadata.code.as_deref(), Some(OUTPUT_STARTED_ERROR_CODE));
        }
        other => panic!("expected a provider error, got {other:?}"),
    }
}

/// Local telemetry sink that records recovery observations without global state.
#[derive(Default)]
struct LocalRecoveryTelemetry {
    replay_prohibited: AtomicUsize,
    http_fallback: AtomicUsize,
}

impl ResponsesRecoveryTelemetry for LocalRecoveryTelemetry {
    fn record(&self, observation: ResponsesRecoveryObservation) {
        match observation {
            ResponsesRecoveryObservation::ReplayProhibited => {
                self.replay_prohibited.fetch_add(1, Ordering::Relaxed);
            }
            ResponsesRecoveryObservation::HttpFallback => {
                self.http_fallback.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Prove the replay wrapper records once through the injected port.
#[test]
fn replay_prohibition_records_through_the_injected_sink() {
    let telemetry = LocalRecoveryTelemetry::default();
    let converted = prohibit_websocket_replay(
        &telemetry,
        LLMError::Provider {
            message: "sensitive provider detail".to_string(),
            metadata: None,
        },
    );

    assert_eq!(
        telemetry.replay_prohibited.load(Ordering::Relaxed),
        1,
        "the wrapper must record exactly one replay prohibition"
    );
    assert_eq!(telemetry.http_fallback.load(Ordering::Relaxed), 0, "a replay prohibition is not an HTTP fallback");
    assert!(websocket_error_prohibits_replay(&converted));
}

/// Captures rendered event fields so a test can assert exactly what a
/// production `tracing::debug!` record contains.
struct CapturedEvents {
    events: Arc<Mutex<Vec<String>>>,
}

impl Subscriber for CapturedEvents {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = String::new();
        event.record(&mut CapturedFields(&mut fields));
        self.events.lock().expect("captured event lock").push(fields);
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

struct CapturedFields<'a>(&'a mut String);

impl Visit for CapturedFields<'_> {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        writeln!(self.0, "{}={value:?}", field.name()).expect("writing to a String cannot fail");
    }
}

/// Prove the fallback path records through the injected sink and logs a stable
/// category without the raw provider diagnostic.
#[test]
fn http_fallback_records_through_the_injected_sink_without_provider_text() {
    const PROVIDER_MARKER: &str = "PROVIDER_BODY_MARKER_9f21c7";
    let events = Arc::new(Mutex::new(Vec::new()));
    let subscriber = CapturedEvents { events: Arc::clone(&events) };
    let telemetry = LocalRecoveryTelemetry::default();
    let error = LLMError::Provider {
        message: format!("upstream said {PROVIDER_MARKER}"),
        metadata: None,
    };

    tracing::subscriber::with_default(subscriber, || {
        record_websocket_http_fallback(&telemetry, "gpt-5", &error);
    });

    assert_eq!(
        telemetry.http_fallback.load(Ordering::Relaxed),
        1,
        "the fallback path must record exactly one HTTP fallback"
    );
    assert_eq!(
        telemetry.replay_prohibited.load(Ordering::Relaxed),
        0,
        "an HTTP fallback is not a replay prohibition"
    );
    let events = events.lock().expect("captured event lock");
    let logs = events.join("\n");
    assert!(logs.contains("responses_websocket_http_fallback"), "{logs}");
    assert!(logs.contains("error_category="), "{logs}");
    assert!(logs.contains("model=gpt-5"), "{logs}");
    assert!(!logs.contains(PROVIDER_MARKER), "provider detail leaked into logs: {logs}");
}

/// Prove the production sink reaches both process counters.
#[test]
fn process_recovery_telemetry_increments_both_recovery_counters() {
    let before = responses_websocket_metrics();
    let telemetry = process_responses_recovery_telemetry();
    telemetry.record(ResponsesRecoveryObservation::ReplayProhibited);
    telemetry.record(ResponsesRecoveryObservation::HttpFallback);
    let after = responses_websocket_metrics();

    // Nextest runs each test in its own process, but the counter is still
    // process-wide, so a lower bound is the strongest portable assertion.
    assert!(
        after.replay_prohibited_total > before.replay_prohibited_total,
        "replay prohibition must increment the replay_prohibited_total counter"
    );
    assert!(
        after.http_fallback_total > before.http_fallback_total,
        "HTTP fallback must increment the http_fallback_total counter"
    );
}

#[test]
fn websocket_url_is_derived_from_http_base() {
    let ws = responses_websocket_url("https://api.openai.com/v1").expect("websocket url should be built");
    assert_eq!(ws, "wss://api.openai.com/v1/responses");
}

#[test]
fn websocket_beta_header_prefers_v2_protocol() {
    assert_eq!(OPENAI_BETA_RESPONSES_WEBSOCKET_V2, "responses=v2");
}

#[test]
fn websocket_connection_limit_error_is_detected() {
    let err = LLMError::Network {
        message: format!("OpenAI error: {WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE}: limit reached"),
        metadata: None,
    };
    assert!(is_websocket_connection_limit_error(&err));
}

#[test]
fn websocket_non_connection_limit_error_is_not_detected() {
    let err = LLMError::Provider {
        message: "OpenAI error: invalid_request".to_string(),
        metadata: None,
    };
    assert!(!is_websocket_connection_limit_error(&err));
}

#[test]
fn websocket_active_response_error_is_detected() {
    let err = LLMError::Provider {
        message: format!("OpenAI error: invalid_request_error: {WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX} resp_active"),
        metadata: None,
    };
    assert!(is_websocket_active_response_error(&err));
}

#[test]
fn websocket_non_active_response_error_is_not_detected() {
    let err = LLMError::Provider {
        message: "OpenAI error: invalid_request_error: something else".to_string(),
        metadata: None,
    };
    assert!(!is_websocket_active_response_error(&err));
}

#[test]
fn websocket_previous_response_not_found_error_is_detected() {
    let err = LLMError::Provider {
        message: format!("OpenAI error: {PREVIOUS_RESPONSE_NOT_FOUND_CODE}: previous response missing"),
        metadata: None,
    };
    assert!(is_websocket_previous_response_not_found_error(&err));
}

#[test]
fn websocket_incremental_input_allows_empty_delta_for_v2_chaining() {
    let input = vec![Value::String("a".to_string())];
    assert!(input_is_incremental(&input, &input));
}

#[test]
fn websocket_incremental_input_requires_prefix_match() {
    let previous = vec![Value::String("a".to_string())];
    let current = vec![Value::String("b".to_string())];
    assert!(!input_is_incremental(&previous, &current));
}

proptest! {
    /// Prove continuation never crosses a changed model, tool, or history context.
    #[test]
    fn websocket_continuation_never_crosses_changed_context(
        model in "[a-z]{1,16}", instructions in ".{0,32}", tool_name in "[a-z]{1,16}",
        change in 0_u8..4,
    ) {
        let input = vec![json!({"role":"user","content":"fixture"})];
        let tools = json!([{"type":"function","name":tool_name}]);
        let cache = OpenAIResponsesWebSocketContinuationCache {
            response_id:"fixture_response".into(), full_input:input.clone(), model:model.clone(),
            instructions:Some(instructions.clone()), tools:Some(tools.clone()),
        };
        let mut payload = json!({"model":model,"instructions":instructions,"tools":tools,"input":input});
        prop_assert!(cache.can_continue_from(&payload));
        match change {
            0 => payload["model"] = json!(format!("other-{model}")),
            1 => payload["instructions"] = json!(format!("other-{instructions}")),
            2 => payload["tools"][0]["name"] = json!(format!("other-{tool_name}")),
            _ => payload["input"][0]["content"] = json!("different conversation"),
        }
        prop_assert!(!cache.can_continue_from(&payload));
        let prepared = prepare_websocket_event(&payload, Some(&cache), false)
            .expect("websocket event prepares");
        prop_assert!(!prepared.used_previous_response_id);
        prop_assert!(prepared.event.get("previous_response_id").is_none());
        prop_assert_eq!(prepared.event.get("input"), payload.get("input"));
    }
}

#[test]
fn websocket_warmup_sets_generate_false() {
    let mut obj = Map::new();
    apply_generate_mode(&mut obj, true);
    assert_eq!(obj.get("generate"), Some(&Value::Bool(false)));
}

#[test]
fn websocket_non_warmup_removes_generate_flag() {
    let mut obj = Map::new();
    obj.insert("generate".to_string(), Value::Bool(false));
    apply_generate_mode(&mut obj, false);
    assert!(obj.get("generate").is_none());
}

#[test]
fn websocket_prepare_event_starts_new_chain_without_continuation() {
    let payload = json!({
        "model": "gpt-5.6",
        "input": [{"role": "user", "content": "hello"}],
        "previous_response_id": "resp_prev",
        "store": true,
        "stream": false,
        "background": false,
    });

    let prepared = prepare_websocket_event(&payload, None, false).expect("event should prepare");

    assert_eq!(
        prepared.event.get("previous_response_id"),
        None,
        "fresh websocket chain should not reuse caller-supplied response ids"
    );
    assert_eq!(prepared.event.get("input").and_then(Value::as_array), Some(&prepared.full_input));
    assert_eq!(prepared.event.get("store").and_then(Value::as_bool), Some(false));
    assert!(!prepared.used_previous_response_id);
}

#[test]
fn websocket_prepare_event_reuses_matching_continuation_incrementally() {
    let full_input = vec![
        json!({"role": "user", "content": "hello"}),
        json!({"role": "user", "content": "continue"}),
    ];
    let continuation = OpenAIResponsesWebSocketContinuationCache {
        response_id: "resp_prev".to_string(),
        full_input: vec![full_input[0].clone()],
        model: "gpt-5.6".to_string(),
        instructions: None,
        tools: None,
    };
    let payload = json!({
        "model": "gpt-5.6",
        "input": full_input,
    });

    let prepared = prepare_websocket_event(&payload, Some(&continuation), false).expect("event");

    assert_eq!(prepared.event.get("previous_response_id").and_then(Value::as_str), Some("resp_prev"));
    assert_eq!(
        prepared.event.get("input").and_then(Value::as_array),
        Some(&vec![json!({"role": "user", "content": "continue"})])
    );
    assert!(prepared.used_previous_response_id);
}

#[test]
fn websocket_prepare_event_starts_new_chain_on_non_prefix_input() {
    let continuation = OpenAIResponsesWebSocketContinuationCache {
        response_id: "resp_prev".to_string(),
        full_input: vec![json!({"role": "user", "content": "hello"})],
        model: "gpt-5.6".to_string(),
        instructions: None,
        tools: None,
    };
    let payload = json!({
        "model": "gpt-5.6",
        "input": [json!({"role": "user", "content": "different"})],
        "previous_response_id": "resp_prev",
    });

    let prepared = prepare_websocket_event(&payload, Some(&continuation), false).expect("event");

    assert!(prepared.event.get("previous_response_id").is_none());
    assert_eq!(prepared.event.get("input").and_then(Value::as_array), Some(&prepared.full_input));
    assert_eq!(prepared.event.get("store").and_then(Value::as_bool), Some(false));
    assert!(!prepared.used_previous_response_id);
}
