//! Shared fixtures for the Responses WebSocket behavioural tests.

use crate::provider::{LLMRequest, Message as ProviderMessage};
use crate::providers::openai::OpenAIProvider;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex as StdMutex};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use vtcode_config::core::OpenAIConfig;

use super::super::PreparedWebSocketEvent;

#[derive(Clone)]
pub(super) enum ScriptedReply {
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

pub(super) async fn spawn_scripted_websocket_server(
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

pub(super) fn websocket_test_provider(base_url: String) -> OpenAIProvider {
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

pub(super) fn websocket_test_custom_provider(base_url: String) -> OpenAIProvider {
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

pub(super) fn websocket_test_request() -> LLMRequest {
    LLMRequest {
        model: "gpt-5.6".to_string(),
        messages: vec![ProviderMessage::user("hello".to_string())].into(),
        ..Default::default()
    }
}

pub(super) fn seed_continuation_cache(provider: &OpenAIProvider, request: &LLMRequest, response_id: &str, store: bool) {
    let payload = provider
        .convert_to_openai_responses_format(request)
        .expect("payload should serialize");
    let full_input = payload.get("input").and_then(Value::as_array).cloned().expect("full input");
    let mut event = payload;
    event["store"] = Value::Bool(store);

    provider.update_websocket_continuation(
        &json!({ "id": response_id }),
        request,
        &PreparedWebSocketEvent {
            event,
            full_input,
            used_previous_response_id: false,
        },
        "gpt-5.6",
    );
}
