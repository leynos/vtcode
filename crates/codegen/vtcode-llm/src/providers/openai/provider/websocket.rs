//! Responses WebSocket transport assembly: continuation, error, recovery, and socket modules.
//!
//! The provider-side orchestration lives here so the transport-specific
//! helpers stay grouped by responsibility in the child modules.

use super::OpenAIProvider;
use crate::provider::{LLMError, LLMRequest, LLMResponse};
use serde_json::Value;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

mod continuation;
mod error;
mod recovery;
mod transport;

pub(super) use continuation::OpenAIResponsesWebSocketContinuationCache;
pub(super) use error::websocket_error_prohibits_replay;
pub(super) use transport::OpenAIResponsesWebSocketSession;

use continuation::{ContinuationLease, PreparedWebSocketEvent, prepare_websocket_event};
use error::{
    OPENAI_BETA_RESPONSES_WEBSOCKET_V2, format_network_error, format_provider_error,
    is_websocket_active_response_error, is_websocket_auth_retryable, is_websocket_previous_response_not_found_error,
    is_websocket_reconnect_error,
};
use recovery::{ProcessWebSocketRecoveryTelemetry, prohibit_websocket_replay};
use transport::{responses_websocket_url, send_websocket_event};

#[cfg(test)]
use continuation::{apply_generate_mode, input_is_incremental};
#[cfg(test)]
use error::{
    OUTPUT_STARTED_ERROR_CODE, PREVIOUS_RESPONSE_NOT_FOUND_CODE, WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX,
    WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE, is_websocket_connection_limit_error, responses_output_started_error,
};
#[cfg(test)]
use recovery::WebSocketRecoveryTelemetry;

impl OpenAIProvider {
    pub(super) async fn generate_via_responses_websocket(&self, request: &LLMRequest) -> Result<LLMResponse, LLMError> {
        let payload = self.convert_to_openai_responses_format(request)?;
        let telemetry = ProcessWebSocketRecoveryTelemetry;
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
                            return Err(prohibit_websocket_replay(&telemetry, err));
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
                        .map_err(|err| prohibit_websocket_replay(&telemetry, err))?;
                    self.update_websocket_continuation(&response_json, request, &prepared, &self.model);
                    *session_guard = Some(session);
                    continuation_lease.completed = true;
                    return Ok(parsed);
                }
                Err(err) => {
                    *session_guard = None;
                    self.clear_nonpersistent_websocket_continuation();
                    if session.output_started {
                        return Err(prohibit_websocket_replay(&telemetry, err));
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

#[cfg(test)]
mod tests;
