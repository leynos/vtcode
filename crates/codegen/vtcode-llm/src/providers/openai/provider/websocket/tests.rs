//! Behavioural coverage for the optional Responses WebSocket transport.
//!
//! The tests cover replay prohibition, URL derivation, continuation cache and
//! incremental-input policy, and scripted-server recovery paths. They drive the
//! same production helpers and provider type as the transport module.

use super::{
    OPENAI_BETA_RESPONSES_WEBSOCKET_V2, OUTPUT_STARTED_ERROR_CODE, OpenAIResponsesWebSocketContinuationCache,
    PREVIOUS_RESPONSE_NOT_FOUND_CODE, ProcessWebSocketRecoveryTelemetry, WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX,
    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, WebSocketRecoveryTelemetry, apply_generate_mode, input_is_incremental,
    is_websocket_active_response_error, is_websocket_connection_limit_error,
    is_websocket_previous_response_not_found_error, prepare_websocket_event, prohibit_websocket_replay,
    responses_output_started_error, responses_websocket_url, websocket_error_prohibits_replay,
};
use crate::provider::LLMError;
use crate::provider::{LLMProvider, LLMRequest, Message as ProviderMessage};
use crate::providers::openai::OpenAIProvider;
use crate::providers::shared::responses_websocket_metrics;
use futures::{SinkExt, StreamExt};
use proptest::prelude::*;
use serde_json::{Map, Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use vtcode_config::core::OpenAIConfig;

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

/// Local telemetry sink that records replay prohibitions without global state.
#[derive(Default)]
struct LocalRecoveryTelemetry {
    replay_prohibited: AtomicUsize,
}

impl WebSocketRecoveryTelemetry for LocalRecoveryTelemetry {
    fn replay_prohibited(&self) {
        self.replay_prohibited.fetch_add(1, Ordering::Relaxed);
    }
}

/// Prove the orchestration wrapper records once through the injected port.
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
    assert!(websocket_error_prohibits_replay(&converted));
}

/// Prove the production telemetry implementation reaches the process counters.
#[test]
fn process_recovery_telemetry_increments_the_replay_counter() {
    let before = responses_websocket_metrics().replay_prohibited_total;
    ProcessWebSocketRecoveryTelemetry.replay_prohibited();
    let after = responses_websocket_metrics().replay_prohibited_total;

    // Nextest runs each test in its own process, but the counter is still
    // process-wide, so a lower bound is the strongest portable assertion.
    assert!(after > before, "replay prohibition must increment the replay_prohibited_total counter");
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

#[derive(Clone)]
enum ScriptedReply {
    Completed {
        response_id: &'static str,
        text: &'static str,
    },
    Error {
        code: &'static str,
        message: &'static str,
    },
    Close {
        reason: &'static str,
    },
    PartialThenClose,
}

async fn spawn_scripted_websocket_server(
    sessions: Vec<Vec<ScriptedReply>>,
) -> Option<(String, Arc<StdMutex<Vec<Value>>>, tokio::task::JoinHandle<()>)> {
    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(listener) => listener,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => return None,
        Err(err) => panic!("listener should bind: {err}"),
    };
    let addr = listener.local_addr().expect("listener addr");
    let recorded = Arc::new(StdMutex::new(Vec::new()));
    let recorded_handle = Arc::clone(&recorded);
    let (ready_tx, ready_rx) = oneshot::channel();

    let handle = tokio::spawn(async move {
        let _ = ready_tx.send(());
        for session_script in sessions {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut websocket = accept_async(stream).await.expect("handshake");
            for reply in session_script {
                let message = websocket
                    .next()
                    .await
                    .expect("request should be present")
                    .expect("request should parse");
                let Message::Text(text) = message else {
                    panic!("expected text websocket payload");
                };
                let payload: Value = serde_json::from_str(text.as_ref()).expect("payload should be valid json");
                recorded_handle.lock().expect("recorded lock").push(payload);

                match reply {
                    ScriptedReply::PartialThenClose => {
                        websocket
                            .send(Message::Text(
                                json!({"type":"response.output_text.delta","delta":"partial"})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .expect("partial output");
                        tokio::task::yield_now().await;
                        websocket.send(Message::Close(None)).await.expect("close after partial");
                        break;
                    }
                    ScriptedReply::Completed { response_id, text } => {
                        let event = json!({
                            "type": "response.completed",
                            "response": {
                                "id": response_id,
                                "output": [{
                                    "type": "message",
                                    "content": [{
                                        "type": "output_text",
                                        "text": text,
                                    }]
                                }]
                            }
                        });
                        websocket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .expect("response event");
                    }
                    ScriptedReply::Error { code, message } => {
                        let event = json!({
                            "type": "error",
                            "status": 400,
                            "error": {
                                "code": code,
                                "message": message,
                            }
                        });
                        websocket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .expect("error event");
                        break;
                    }
                    ScriptedReply::Close { reason } => {
                        websocket
                            .send(Message::Close(Some(CloseFrame {
                                code: CloseCode::Away,
                                reason: reason.to_string().into(),
                            })))
                            .await
                            .expect("close frame");
                        break;
                    }
                }
            }
        }
    });

    ready_rx.await.expect("server ready");
    Some((format!("http://api.openai.com@127.0.0.1:{}/v1", addr.port()), recorded, handle))
}

fn websocket_test_provider(base_url: String) -> OpenAIProvider {
    OpenAIProvider::from_config(
        Some("test-key".to_string()),
        None,
        Some("gpt-5.6".to_string()),
        Some(base_url),
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    )
}

fn websocket_test_custom_provider(base_url: String) -> OpenAIProvider {
    OpenAIProvider::from_custom_config(
        "mycorp".to_string(),
        "MyCorp".to_string(),
        Some("test-key".to_string()),
        Some("gpt-5.6".to_string()),
        Some(base_url),
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
        None,
        None,
    )
}

fn websocket_test_request() -> LLMRequest {
    LLMRequest {
        model: "gpt-5.6".to_string(),
        messages: vec![ProviderMessage::user("hello".to_string())].into(),
        ..Default::default()
    }
}

/// Prove a post-output failure never reconnects or falls back to HTTP.
#[tokio::test]
async fn websocket_partial_output_failure_never_reconnects_or_falls_back_to_http() {
    let (base_url, recorded, server) = spawn_scripted_websocket_server(vec![vec![
        ScriptedReply::Completed { response_id: "warmup", text: "" },
        ScriptedReply::PartialThenClose,
    ]])
    .await
    .expect("local server");
    let provider = websocket_test_custom_provider(base_url);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        LLMProvider::generate(&provider, websocket_test_request()),
    )
    .await
    .expect("must not start HTTP fallback")
    .expect_err("partial stream is incomplete");
    assert!(websocket_error_prohibits_replay(&error));
    assert_eq!(recorded.lock().expect("recorded requests lock is not poisoned").len(), 2);
    assert!(provider.websocket_session.lock().await.is_none());
    assert!(provider.websocket_continuation_snapshot().is_none());
    server.await.expect("fixture server completes");
}

/// Prove cancelling a pending read discards the socket and continuation.
#[tokio::test]
async fn websocket_cancelled_pending_read_discards_socket_and_continuation() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("fixture listener binds");
    let base_url =
        format!("http://127.0.0.1:{}/v1", listener.local_addr().expect("fixture listener has an address").port());
    let (observed_tx, observed_rx) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("fixture server accepts a connection");
        let mut socket = accept_async(stream).await.expect("fixture handshake completes");
        let _request = socket
            .next()
            .await
            .expect("fixture client sends a frame")
            .expect("frame is not an error");
        observed_tx.send(()).expect("observer receives the first frame");
        // Cancellation closes this socket; no late response can reach the next turn.
        let closed = socket.next().await;
        assert!(closed.is_none() || matches!(closed, Some(Err(_)) | Some(Ok(Message::Close(_)))));
        let (stream, _) = listener.accept().await.expect("fixture server accepts a connection");
        let mut socket = accept_async(stream).await.expect("fixture handshake completes");
        for (id, text) in [("new_warmup", ""), ("new_final", "fresh response")] {
            let Message::Text(request) = socket
                .next()
                .await
                .expect("fixture client sends a frame")
                .expect("frame is not an error")
            else {
                panic!("text request")
            };
            let request: Value = serde_json::from_str(&request).expect("fixture request is valid JSON");
            assert_ne!(request.get("previous_response_id"), Some(&json!("abandoned")));
            socket.send(Message::Text(json!({"type":"response.completed","response":{"id":id,"output":[{"type":"message","content":[{"type":"output_text","text":text}]}]}}).to_string().into())).await.expect("fixture server sends the completion");
        }
    });
    let provider = websocket_test_custom_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "abandoned", false);
    {
        let pending = provider.generate_via_responses_websocket(&request);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("pending read unexpectedly completed: {result:?}"),
            result = observed_rx => result.expect("observer receives the first frame"),
        }
    }
    assert!(provider.websocket_session.lock().await.is_none());
    assert!(provider.websocket_continuation_snapshot().is_none());
    let response =
        tokio::time::timeout(std::time::Duration::from_secs(5), provider.generate_via_responses_websocket(&request))
            .await
            .expect("reconnect completes within the deadline")
            .expect("reconnect returns a response");
    assert_eq!(response.content.as_deref(), Some("fresh response"));
    server.await.expect("fixture server completes");
}

fn seed_continuation_cache(provider: &OpenAIProvider, request: &LLMRequest, response_id: &str, store: bool) {
    let payload = provider
        .convert_to_openai_responses_format(request)
        .expect("payload should serialize");
    let full_input = payload.get("input").and_then(Value::as_array).cloned().expect("full input");
    let mut event = payload;
    event["store"] = Value::Bool(store);

    provider.update_websocket_continuation(
        &json!({ "id": response_id }),
        request,
        &super::PreparedWebSocketEvent {
            event,
            full_input,
            used_previous_response_id: false,
        },
        "gpt-5.6",
    );
}

#[test]
fn websocket_cache_does_not_affect_normal_http_sse_payload() {
    let provider = websocket_test_provider("https://api.openai.com/v1".to_string());
    let mut request = websocket_test_request();
    request.previous_response_id = Some("resp_http_caller".to_string());
    request.response_store = Some(true);
    seed_continuation_cache(&provider, &request, "resp_cached", false);

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("normal responses payload should build");

    assert!(payload.get("previous_response_id").is_none());
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
}

#[tokio::test]
async fn websocket_reconnects_after_connection_limit_error() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE,
            message: "Responses websocket connection limit reached (60 minutes).",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed { response_id: "resp_reconnected", text: "ok" },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("websocket retry should succeed");

    assert_eq!(response.content.as_deref(), Some("ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
        assert!(
            recorded
                .iter()
                .all(|payload| payload.get("store").and_then(Value::as_bool) == Some(false)),
            "websocket requests must enforce store=false"
        );
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_custom_provider_uses_compatible_transport() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![vec![
        ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
        ScriptedReply::Completed { response_id: "resp_final", text: "compat ok" },
    ]])
    .await
    else {
        return;
    };
    let compatible_base_url = base_url.replacen("api.openai.com", "compat.example", 1);
    let provider = websocket_test_custom_provider(compatible_base_url);

    let response = LLMProvider::generate(&provider, websocket_test_request())
        .await
        .expect("custom compatible provider should use websocket mode");

    assert_eq!(response.content.as_deref(), Some("compat ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0].get("generate").and_then(Value::as_bool), Some(false));
        assert!(recorded[0].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_reconnects_after_active_response_error() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: "invalid_request_error",
            message: "Conversation already has an active response in progress: resp_active.",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed {
                response_id: "resp_after_active",
                text: "retried ok",
            },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("active-response retry should succeed");

    assert_eq!(response.content.as_deref(), Some("retried ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_failed_turn_drops_nonpersistent_continuation_before_retry() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: "invalid_request_error",
            message: "Conversation already has an active response in progress: resp_active.",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed { response_id: "resp_after_reset", text: "reset ok" },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", false);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("failed turn should restart nonpersistent continuation cleanly");

    assert_eq!(response.content.as_deref(), Some("reset ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_previous_response_not_found_restarts_new_chain() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Error {
            code: PREVIOUS_RESPONSE_NOT_FOUND_CODE,
            message: "Previous response with id 'resp_cached' not found.",
        }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed { response_id: "resp_final", text: "new chain ok" },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("provider should recover by starting a new chain");

    assert_eq!(response.content.as_deref(), Some("new chain ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_reconnects_after_close_frame() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Close { reason: "limit reached" }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed {
                response_id: "resp_after_close",
                text: "closed then ok",
            },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", true);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("close-frame retry should succeed");

    assert_eq!(response.content.as_deref(), Some("closed then ok"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}

#[tokio::test]
async fn websocket_reconnect_drops_nonpersistent_continuation_before_retry() {
    let Some((base_url, recorded, handle)) = spawn_scripted_websocket_server(vec![
        vec![ScriptedReply::Close { reason: "network reset" }],
        vec![
            ScriptedReply::Completed { response_id: "resp_warmup", text: "" },
            ScriptedReply::Completed {
                response_id: "resp_final",
                text: "restarted cleanly",
            },
        ],
    ])
    .await
    else {
        return;
    };
    let provider = websocket_test_provider(base_url);
    let request = websocket_test_request();
    seed_continuation_cache(&provider, &request, "resp_cached", false);

    let response = LLMProvider::generate(&provider, request)
        .await
        .expect("nonpersistent reconnect should restart the chain");

    assert_eq!(response.content.as_deref(), Some("restarted cleanly"));
    {
        let recorded = recorded.lock().expect("recorded lock");
        assert_eq!(recorded.len(), 3);
        assert_eq!(recorded[0].get("previous_response_id").and_then(Value::as_str), Some("resp_cached"));
        assert!(recorded[1].get("previous_response_id").is_none());
        assert_eq!(recorded[1].get("generate").and_then(Value::as_bool), Some(false));
        assert_eq!(recorded[2].get("previous_response_id").and_then(Value::as_str), Some("resp_warmup"));
    }
    handle.await.expect("server task");
}
