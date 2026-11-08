//! Socket transport and response framing for the Responses WebSocket protocol.

use crate::provider::LLMError;
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::error::{WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, format_network_error, format_provider_error};

type ResponsesSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

#[derive(Debug)]
pub(crate) struct OpenAIResponsesWebSocketSession {
    socket: ResponsesSocket,
    pub(super) output_started: bool,
}

impl OpenAIResponsesWebSocketSession {
    pub(super) fn new(socket: ResponsesSocket) -> Self {
        Self { socket, output_started: false }
    }
}

pub(super) async fn send_websocket_event(
    session: &mut OpenAIResponsesWebSocketSession,
    event: &Value,
) -> Result<Value, LLMError> {
    session.output_started = false;
    session
        .socket
        .send(Message::Text(event.to_string().into()))
        .await
        .map_err(|err| format_network_error(format!("Failed to send OpenAI WebSocket payload: {err}")))?;
    read_websocket_response(session).await
}

async fn read_websocket_response(session: &mut OpenAIResponsesWebSocketSession) -> Result<Value, LLMError> {
    while let Some(message) = session.socket.next().await {
        let message = message.map_err(|err| format_network_error(format!("OpenAI WebSocket receive failed: {err}")))?;

        match message {
            Message::Text(text) => {
                let event: Value = serde_json::from_str(text.as_ref())
                    .map_err(|err| format_provider_error(format!("Invalid OpenAI WebSocket event JSON: {err}")))?;

                let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                match event_type {
                    "response.output_text.delta"
                    | "response.reasoning_text.delta"
                    | "response.reasoning_summary_text.delta"
                    | "response.function_call_arguments.delta"
                    | "response.custom_tool_call_input.delta" => {
                        session.output_started |= event
                            .get("delta")
                            .and_then(Value::as_str)
                            .is_some_and(|delta| !delta.is_empty());
                    }
                    "response.output_item.added" | "response.output_item.done" => {
                        session.output_started = true;
                    }
                    "response.completed" => {
                        session.output_started = true;
                        if let Some(response) = event.get("response").cloned() {
                            return Ok(response);
                        }
                        return Err(format_provider_error(
                            "OpenAI WebSocket completed event missing response".to_string(),
                        ));
                    }
                    "response.failed" | "response.incomplete" | "error" => {
                        let code = event
                            .get("error")
                            .and_then(|error| error.get("code"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown_error");
                        let message = event
                            .get("error")
                            .or_else(|| event.pointer("/response/error"))
                            .and_then(|error| error.get("message"))
                            .and_then(Value::as_str)
                            .or_else(|| event.pointer("/response/incomplete_details/reason").and_then(Value::as_str))
                            .unwrap_or("OpenAI WebSocket request failed");
                        let formatted = format!("{code}: {message}");
                        if code == WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE {
                            return Err(format_network_error(formatted));
                        }
                        return Err(format_provider_error(formatted));
                    }
                    _ => {}
                }
            }
            Message::Ping(payload) => {
                session
                    .socket
                    .send(Message::Pong(payload))
                    .await
                    .map_err(|err| format_network_error(format!("Failed to reply to OpenAI WebSocket ping: {err}")))?;
            }
            Message::Close(frame) => {
                let reason = frame
                    .map(|frame| frame.reason.to_string())
                    .unwrap_or_else(|| "connection closed".to_string());
                return Err(format_network_error(format!("OpenAI WebSocket connection closed: {reason}")));
            }
            _ => {}
        }
    }

    Err(format_network_error("OpenAI WebSocket stream ended unexpectedly".to_string()))
}

pub(super) fn responses_websocket_url(base_url: &str) -> Result<String, LLMError> {
    let mut url = url::Url::parse(base_url)
        .map_err(|err| format_provider_error(format!("Invalid OpenAI base URL for WebSocket mode: {err}")))?;

    match url.scheme() {
        "https" => {
            let _ = url.set_scheme("wss");
        }
        "http" => {
            let _ = url.set_scheme("ws");
        }
        "wss" | "ws" => {}
        other => {
            return Err(format_provider_error(format!("Unsupported URL scheme for WebSocket mode: {other}")));
        }
    }

    if !url.path().ends_with("/responses") {
        let mut path = url.path().trim_end_matches('/').to_string();
        if path.is_empty() {
            path.push('/');
        }
        path.push_str("/responses");
        url.set_path(&path);
    }

    Ok(url.to_string())
}
