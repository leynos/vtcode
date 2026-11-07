use super::OpenAIProvider;
use crate::error_display;
use crate::provider::{LLMError, LLMErrorMetadata, LLMRequest, LLMResponse};
use crate::providers::shared::record_responses_replay_prohibited;
use futures::{SinkExt, StreamExt};
use serde_json::{Map, Value, json};
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

type ResponsesSocket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX: &str = "Conversation already has an active response in progress:";
const OPENAI_BETA_RESPONSES_WEBSOCKET_V2: &str = "responses=v2";
const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";
const WEBSOCKET_AUTH_RETRY_STATUSES: [&str; 2] = ["401", "403"];
const OUTPUT_STARTED_ERROR_CODE: &str = "responses_output_started";

/// Return whether the error proves that output already began, prohibiting replay.
pub(super) fn websocket_error_prohibits_replay(error: &LLMError) -> bool {
    matches!(error, LLMError::Provider { metadata: Some(metadata), .. }
        if metadata.code.as_deref() == Some(OUTPUT_STARTED_ERROR_CODE))
}

/// Convert a post-output failure into a non-replayable provider error.
///
/// Conversion is pure so its mapping can be tested without touching the
/// process-wide recovery counters. [`prohibit_websocket_replay`] is the
/// orchestration wrapper that records telemetry before calling this.
fn responses_output_started_error(error: LLMError) -> LLMError {
    LLMError::Provider {
        message: format!("Responses WebSocket failed after output began; automatic replay is disabled: {error}"),
        metadata: Some(LLMErrorMetadata::new(
            "OpenAI",
            None,
            Some(OUTPUT_STARTED_ERROR_CODE.to_string()),
            None,
            None,
            None,
            None,
        )),
    }
}

/// Record and convert a post-output failure that must not be replayed.
fn prohibit_websocket_replay(error: LLMError) -> LLMError {
    record_responses_replay_prohibited();
    tracing::warn!(
        target = "vtcode::llm::responses::websocket",
        outcome = "replay_prohibited",
        error_category = "responses_websocket_output_started",
        "Responses WebSocket failed after output began; automatic replay is disabled"
    );
    responses_output_started_error(error)
}

// Retained custom WebSocket transport while VTCode migrates the optional path
// to Rig. Rig 0.40 exposes `client.responses_websocket()` behind its
// `websocket` feature, including warm-up requests, but it does not yet cover
// VTCode's custom auth refresh and retry policy. Keep this path separate from
// normal HTTP/SSE request shaping: it may use `previous_response_id` only as
// connection-local state, and every websocket request still sends `store=false`.
fn is_websocket_active_response_error(err: &LLMError) -> bool {
    let message = match err {
        LLMError::Provider { message, .. } | LLMError::Network { message, .. } => message,
        LLMError::Authentication { .. } | LLMError::RateLimit { .. } | LLMError::InvalidRequest { .. } => return false,
    };

    message.contains(WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX)
}

#[cfg(test)]
pub(super) fn is_websocket_connection_limit_error(err: &LLMError) -> bool {
    let message = match err {
        LLMError::Provider { message, .. } | LLMError::Network { message, .. } => message,
        LLMError::Authentication { .. } | LLMError::RateLimit { .. } | LLMError::InvalidRequest { .. } => return false,
    };

    message.contains(WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE)
}

pub(super) fn is_websocket_previous_response_not_found_error(err: &LLMError) -> bool {
    let message = match err {
        LLMError::Provider { message, .. } | LLMError::Network { message, .. } => message,
        LLMError::Authentication { .. } | LLMError::RateLimit { .. } | LLMError::InvalidRequest { .. } => return false,
    };

    message.contains(PREVIOUS_RESPONSE_NOT_FOUND_CODE)
}

fn is_websocket_reconnect_error(err: &LLMError) -> bool {
    matches!(err, LLMError::Network { .. })
}

fn is_websocket_auth_retryable(error: &tokio_tungstenite::tungstenite::Error) -> bool {
    let message = error.to_string();
    WEBSOCKET_AUTH_RETRY_STATUSES.iter().any(|status| message.contains(status))
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenAIResponsesWebSocketContinuationCache {
    response_id: String,
    full_input: Vec<Value>,
    model: String,
    instructions: Option<String>,
    tools: Option<Value>,
}

impl OpenAIResponsesWebSocketContinuationCache {
    fn from_response(
        response_json: &Value,
        request: &LLMRequest,
        prepared: &PreparedWebSocketEvent,
        fallback_model: &str,
    ) -> Option<Self> {
        let response_id = response_json
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?
            .to_string();

        let model = if request.model.trim().is_empty() {
            fallback_model.to_string()
        } else {
            request.model.clone()
        };
        let instructions = response_json
            .get("instructions")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| prepared.event.get("instructions").and_then(Value::as_str).map(str::to_owned));
        let tools = prepared.event.get("tools").cloned();
        Some(Self {
            response_id,
            full_input: prepared.full_input.clone(),
            model,
            instructions,
            tools,
        })
    }

    fn can_resume_after_reconnect(&self) -> bool {
        false
    }

    fn can_continue_from(&self, payload: &Value) -> bool {
        if self.response_id.is_empty() {
            return false;
        }

        let Some(current_model) = payload.get("model").and_then(Value::as_str) else {
            return false;
        };
        if self.model != current_model {
            return false;
        }

        let current_instructions = payload.get("instructions").and_then(Value::as_str).map(str::to_owned);
        if self.instructions != current_instructions {
            return false;
        }

        let current_tools = payload.get("tools").cloned();
        if self.tools != current_tools {
            return false;
        }

        let Some(current_input) = payload.get("input").and_then(Value::as_array) else {
            return false;
        };
        input_is_incremental(self.full_input.as_slice(), current_input.as_slice())
    }
}

#[derive(Debug)]
pub(crate) struct OpenAIResponsesWebSocketSession {
    socket: ResponsesSocket,
    output_started: bool,
}

impl OpenAIResponsesWebSocketSession {
    fn new(socket: ResponsesSocket) -> Self {
        Self { socket, output_started: false }
    }
}

#[derive(Debug)]
struct PreparedWebSocketEvent {
    event: Value,
    full_input: Vec<Value>,
    used_previous_response_id: bool,
}

/// Continuation belongs to the completed exchange, never an abandoned future.
struct ContinuationLease<'a> {
    provider: &'a OpenAIProvider,
    completed: bool,
}

impl Drop for ContinuationLease<'_> {
    // Invalidate continuation unless the exchange completed.
    fn drop(&mut self) {
        if !self.completed {
            self.provider.clear_websocket_continuation();
        }
    }
}

impl OpenAIProvider {
    pub(super) async fn generate_via_responses_websocket(&self, request: &LLMRequest) -> Result<LLMResponse, LLMError> {
        let payload = self.convert_to_openai_responses_format(request)?;
        let mut retried_active_response = false;
        let mut retried_reconnect = false;
        let mut retried_new_chain = false;

        loop {
            let needs_warmup = self.websocket_continuation_snapshot().is_none();
            let mut session_guard = self.websocket_session.lock().await;
            self.ensure_websocket_session(&mut session_guard, request).await?;
            // A cancelled future must drop the in-flight socket, not return it
            // to another turn with unread events still queued on the wire.
            let mut session = session_guard
                .take()
                .ok_or_else(|| format_provider_error("OpenAI WebSocket session unexpectedly missing".to_string()))?;
            let mut continuation_lease = ContinuationLease { provider: self, completed: false };

            if needs_warmup {
                let warmup_prepared = prepare_websocket_event(&payload, None, true)?;
                match send_websocket_event(&mut session, &warmup_prepared.event).await {
                    Ok(response_json) => {
                        self.update_websocket_continuation(&response_json, request, &warmup_prepared, &self.model);
                    }
                    Err(err) => {
                        *session_guard = None;
                        self.clear_nonpersistent_websocket_continuation();
                        if session.output_started {
                            return Err(prohibit_websocket_replay(err));
                        }
                        if !retried_reconnect && is_websocket_reconnect_error(&err) {
                            retried_reconnect = true;
                            continue;
                        }
                        return Err(err);
                    }
                }
            }

            let continuation = self.websocket_continuation_snapshot();
            let prepared = prepare_websocket_event(&payload, continuation.as_ref(), false)?;

            match send_websocket_event(&mut session, &prepared.event).await {
                Ok(response_json) => {
                    let parsed = self
                        .parse_openai_responses_response(response_json.clone(), request.model.clone())
                        .map_err(prohibit_websocket_replay)?;
                    self.update_websocket_continuation(&response_json, request, &prepared, &self.model);
                    *session_guard = Some(session);
                    continuation_lease.completed = true;
                    return Ok(parsed);
                }
                Err(err) => {
                    *session_guard = None;
                    self.clear_nonpersistent_websocket_continuation();
                    if session.output_started {
                        return Err(prohibit_websocket_replay(err));
                    }
                    if !retried_active_response && is_websocket_active_response_error(&err) {
                        tracing::warn!(
                            model = %request.model,
                            "OpenAI Responses websocket rejected response.create while another response was still active; reconnecting once"
                        );
                        retried_active_response = true;
                        continue;
                    }
                    if !retried_new_chain
                        && prepared.used_previous_response_id
                        && is_websocket_previous_response_not_found_error(&err)
                    {
                        self.clear_websocket_continuation();
                        retried_new_chain = true;
                        continue;
                    }
                    if !retried_reconnect && is_websocket_reconnect_error(&err) {
                        retried_reconnect = true;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    async fn ensure_websocket_session<'a>(
        &self,
        session_guard: &'a mut Option<OpenAIResponsesWebSocketSession>,
        request: &LLMRequest,
    ) -> Result<&'a mut OpenAIResponsesWebSocketSession, LLMError> {
        if session_guard.is_none() {
            let ws_url = responses_websocket_url(&self.base_url)?;
            let build_request = |api_key: &str| -> Result<_, LLMError> {
                let mut ws_request = ws_url
                    .clone()
                    .into_client_request()
                    .map_err(|err| format_provider_error(format!("Invalid OpenAI WebSocket request: {err}")))?;

                ws_request.headers_mut().insert(
                    "Authorization",
                    HeaderValue::from_str(&format!("Bearer {api_key}"))
                        .map_err(|err| format_provider_error(format!("Invalid OpenAI authorization header: {err}")))?,
                );
                ws_request
                    .headers_mut()
                    .insert("OpenAI-Beta", HeaderValue::from_static(OPENAI_BETA_RESPONSES_WEBSOCKET_V2));
                if let Some(metadata) = &request.metadata
                    && let Ok(metadata_str) = serde_json::to_string(metadata)
                    && let Ok(value) = HeaderValue::from_str(&metadata_str)
                {
                    ws_request.headers_mut().insert("X-Turn-Metadata", value);
                }

                Ok(ws_request)
            };

            let api_key = self.current_api_key().await?;
            let ws_request = build_request(&api_key)?;
            let socket = match connect_async(ws_request).await {
                Ok((socket, _)) => socket,
                Err(err) if self.uses_chatgpt_auth() && is_websocket_auth_retryable(&err) => {
                    let retry_api_key = self.refresh_api_key_for_retry().await?;
                    let retry_request = build_request(&retry_api_key)?;
                    let (socket, _) = connect_async(retry_request).await.map_err(|retry_err| {
                        format_network_error(format!("Failed to connect OpenAI WebSocket: {retry_err}"))
                    })?;
                    socket
                }
                Err(err) => {
                    return Err(format_network_error(format!("Failed to connect OpenAI WebSocket: {err}")));
                }
            };
            *session_guard = Some(OpenAIResponsesWebSocketSession::new(socket));
        }

        session_guard
            .as_mut()
            .ok_or_else(|| format_provider_error("OpenAI WebSocket session unexpectedly missing".to_string()))
    }

    fn websocket_continuation_snapshot(&self) -> Option<OpenAIResponsesWebSocketContinuationCache> {
        self.websocket_continuation_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn update_websocket_continuation(
        &self,
        response_json: &Value,
        request: &LLMRequest,
        prepared: &PreparedWebSocketEvent,
        fallback_model: &str,
    ) {
        let cache =
            OpenAIResponsesWebSocketContinuationCache::from_response(response_json, request, prepared, fallback_model);
        *self
            .websocket_continuation_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = cache;
    }

    fn clear_websocket_continuation(&self) {
        *self
            .websocket_continuation_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn clear_nonpersistent_websocket_continuation(&self) {
        let mut cache = self
            .websocket_continuation_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if cache
            .as_ref()
            .is_some_and(|continuation| !continuation.can_resume_after_reconnect())
        {
            *cache = None;
        }
    }
}

fn input_is_incremental(last_input: &[Value], current_input: &[Value]) -> bool {
    if current_input.len() < last_input.len() {
        return false;
    }
    current_input.starts_with(last_input)
}

fn apply_generate_mode(request_obj: &mut Map<String, Value>, warmup: bool) {
    if warmup {
        request_obj.insert("generate".to_string(), Value::Bool(false));
    } else {
        request_obj.remove("generate");
    }
}

fn prepare_websocket_event(
    payload: &Value,
    continuation: Option<&OpenAIResponsesWebSocketContinuationCache>,
    warmup: bool,
) -> Result<PreparedWebSocketEvent, LLMError> {
    let mut request_obj = payload
        .as_object()
        .cloned()
        .ok_or_else(|| format_provider_error("Invalid Responses payload".to_string()))?;

    request_obj.remove("stream");
    request_obj.remove("background");
    request_obj.insert("store".to_string(), Value::Bool(false));
    apply_generate_mode(&mut request_obj, warmup);

    let full_input = request_obj
        .get("input")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| format_provider_error("Responses payload missing input".to_string()))?;

    let mut used_previous_response_id = false;
    if !warmup
        && let Some(continuation) = continuation
        && continuation.can_continue_from(&Value::Object(request_obj.clone()))
    {
        if !continuation.response_id.is_empty() {
            request_obj.insert("previous_response_id".to_string(), Value::String(continuation.response_id.clone()));
            let incremental = full_input[continuation.full_input.len()..].to_vec();
            request_obj.insert("input".to_string(), Value::Array(incremental));
            used_previous_response_id = true;
        }
    } else {
        request_obj.remove("previous_response_id");
    }

    let event = Value::Object(
        std::iter::once(("type".to_string(), Value::String("response.create".to_string())))
            .chain(request_obj)
            .collect(),
    );

    Ok(PreparedWebSocketEvent { event, full_input, used_previous_response_id })
}

async fn send_websocket_event(session: &mut OpenAIResponsesWebSocketSession, event: &Value) -> Result<Value, LLMError> {
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

fn responses_websocket_url(base_url: &str) -> Result<String, LLMError> {
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

#[cold]
fn format_provider_error(message: String) -> LLMError {
    LLMError::Provider {
        message: error_display::format_llm_error("OpenAI", &message),
        metadata: None,
    }
}

#[cold]
fn format_network_error(message: String) -> LLMError {
    LLMError::Network {
        message: error_display::format_llm_error("OpenAI", &message),
        metadata: None,
    }
}

#[cfg(test)]
mod tests;
