//! Recovery-path coverage for the Responses WebSocket transport.

use crate::provider::LLMProvider;
use crate::providers::shared::{ResponsesRecoveryObservation, process_responses_recovery_telemetry};
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;

use super::super::{PREVIOUS_RESPONSE_NOT_FOUND_CODE, websocket_error_prohibits_replay};
use super::LocalRecoveryTelemetry;
use super::support::{
    ScriptedReply, seed_continuation_cache, spawn_scripted_websocket_server, websocket_test_custom_provider,
    websocket_test_provider, websocket_test_request,
};

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

/// Prove the WebSocket orchestration reports a replay prohibition through the
/// injected sink rather than writing the process-wide counter itself.
#[tokio::test]
async fn websocket_partial_output_reports_replay_prohibition_through_the_injected_sink() {
    use std::sync::atomic::Ordering;

    let (base_url, recorded, server) = spawn_scripted_websocket_server(vec![vec![
        ScriptedReply::Completed { response_id: "warmup", text: "" },
        ScriptedReply::PartialThenClose,
    ]])
    .await
    .expect("local server");
    let provider = websocket_test_custom_provider(base_url);
    let telemetry = LocalRecoveryTelemetry::default();

    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.generate_via_responses_websocket(&websocket_test_request(), &telemetry),
    )
    .await
    .expect("must not start HTTP fallback")
    .expect_err("partial stream is incomplete");

    assert!(websocket_error_prohibits_replay(&error));
    assert_eq!(recorded.lock().expect("recorded requests lock is not poisoned").len(), 2);
    assert_eq!(
        telemetry.replay_prohibited.load(Ordering::Relaxed),
        1,
        "the injected sink must receive exactly one replay prohibition"
    );
    assert_eq!(
        telemetry.http_fallback.load(Ordering::Relaxed),
        0,
        "a post-output failure must not report an HTTP fallback"
    );
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
    let process_telemetry = process_responses_recovery_telemetry();
    {
        let pending = provider.generate_via_responses_websocket(&request, &process_telemetry);
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("pending read unexpectedly completed: {result:?}"),
            result = observed_rx => result.expect("observer receives the first frame"),
        }
    }
    assert!(provider.websocket_session.lock().await.is_none());
    assert!(provider.websocket_continuation_snapshot().is_none());
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        provider.generate_via_responses_websocket(&request, &process_telemetry),
    )
    .await
    .expect("reconnect completes within the deadline")
    .expect("reconnect returns a response");
    assert_eq!(response.content.as_deref(), Some("fresh response"));
    server.await.expect("fixture server completes");
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
