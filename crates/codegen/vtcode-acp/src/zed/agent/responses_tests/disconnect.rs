//! Mid-stream provider disconnection handling.
use super::super::provider::{StreamDeadlineTracker, StreamTimeoutPhase};
use super::super::tests::{
    PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers,
};
use super::super::*;
use super::support::*;
use crate::acp::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::routing::post;
use futures::stream;
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::{Notify, mpsc};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::llm::provider::{Message as ProviderMessage, MessageRole as ProviderMessageRole};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Streams one visible text delta, then terminates the response body with a transport error.
async fn partial_disconnect(State(state): State<DisconnectState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let first = Bytes::from_static(
        b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":0,\"delta\":\"visible before disconnect\"}\n\n",
    );
    let body = Body::from_stream(stream::unfold((0_u8, first), |(step, first)| async move {
        match step {
            0 => Some((Ok::<Bytes, std::io::Error>(first.clone()), (1, first))),
            1 => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Some((Err(std::io::Error::other("scripted Responses disconnect")), (2, first)))
            }
            _ => None,
        }
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("partial disconnect response")
}

/// Proves a mid-stream disconnect after visible output is not replayed and marks the turn incomplete.
#[tokio::test]
async fn partial_responses_disconnect_is_not_retried_and_marks_turn_incomplete() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(partial_disconnect))
        .with_state(DisconnectState { requests: Arc::clone(&requests) });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind disconnect server");
    let address = listener.local_addr().expect("disconnect server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve disconnect fixture");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Disconnect after visible output"],
        ACP_PROTOCOL_DEADLINE,
    )
    .await;
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1, "visible output makes automatic replay unsafe");
    assert_eq!(run.active_acp_requests, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert!(visible_text(&run.notifications).contains("visible before disconnect"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("visible before disconnect")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}
