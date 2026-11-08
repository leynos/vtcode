//! Continuation cache and event preparation for the Responses WebSocket transport.

use crate::provider::{LLMError, LLMRequest};
use serde_json::{Map, Value};

use super::super::OpenAIProvider;
use super::error::format_provider_error;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenAIResponsesWebSocketContinuationCache {
    pub(super) response_id: String,
    pub(super) full_input: Vec<Value>,
    pub(super) model: String,
    pub(super) instructions: Option<String>,
    pub(super) tools: Option<Value>,
}

impl OpenAIResponsesWebSocketContinuationCache {
    pub(super) fn from_response(
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

    pub(super) fn can_resume_after_reconnect(&self) -> bool {
        false
    }

    pub(super) fn can_continue_from(&self, payload: &Value) -> bool {
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
pub(super) struct PreparedWebSocketEvent {
    pub(super) event: Value,
    pub(super) full_input: Vec<Value>,
    pub(super) used_previous_response_id: bool,
}

/// Continuation belongs to the completed exchange, never an abandoned future.
pub(super) struct ContinuationLease<'a> {
    pub(super) provider: &'a OpenAIProvider,
    pub(super) completed: bool,
}

impl Drop for ContinuationLease<'_> {
    // Invalidate continuation unless the exchange completed.
    fn drop(&mut self) {
        if !self.completed {
            self.provider.clear_websocket_continuation();
        }
    }
}

pub(super) fn input_is_incremental(last_input: &[Value], current_input: &[Value]) -> bool {
    if current_input.len() < last_input.len() {
        return false;
    }
    current_input.starts_with(last_input)
}

pub(super) fn apply_generate_mode(request_obj: &mut Map<String, Value>, warmup: bool) {
    if warmup {
        request_obj.insert("generate".to_string(), Value::Bool(false));
    } else {
        request_obj.remove("generate");
    }
}

pub(super) fn prepare_websocket_event(
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
