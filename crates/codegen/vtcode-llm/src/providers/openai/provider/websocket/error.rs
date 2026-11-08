//! Error codes, classification, and construction for the Responses WebSocket transport.

use crate::error_display;
use crate::provider::{LLMError, LLMErrorMetadata};

pub(super) const WEBSOCKET_ACTIVE_RESPONSE_ERROR_PREFIX: &str =
    "Conversation already has an active response in progress:";
pub(super) const OPENAI_BETA_RESPONSES_WEBSOCKET_V2: &str = "responses=v2";
pub(super) const WEBSOCKET_CONNECTION_LIMIT_REACHED_CODE: &str = "websocket_connection_limit_reached";
pub(super) const PREVIOUS_RESPONSE_NOT_FOUND_CODE: &str = "previous_response_not_found";
pub(super) const WEBSOCKET_AUTH_RETRY_STATUSES: [&str; 2] = ["401", "403"];
pub(super) const OUTPUT_STARTED_ERROR_CODE: &str = "responses_output_started";

/// Return whether the error proves that output already began, prohibiting replay.
pub(in crate::providers::openai::provider) fn websocket_error_prohibits_replay(error: &LLMError) -> bool {
    matches!(error, LLMError::Provider { metadata: Some(metadata), .. }
        if metadata.code.as_deref() == Some(OUTPUT_STARTED_ERROR_CODE))
}

/// Convert a post-output failure into a non-replayable provider error.
///
/// Conversion is pure so its mapping can be tested without touching the
/// process-wide recovery counters.
/// [`prohibit_websocket_replay`](super::recovery::prohibit_websocket_replay)
/// is the orchestration wrapper that records telemetry before calling this.
pub(super) fn responses_output_started_error(error: LLMError) -> LLMError {
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

// Retained custom WebSocket transport while VTCode migrates the optional path
// to Rig. Rig 0.40 exposes `client.responses_websocket()` behind its
// `websocket` feature, including warm-up requests, but it does not yet cover
// VTCode's custom auth refresh and retry policy. Keep this path separate from
// normal HTTP/SSE request shaping: it may use `previous_response_id` only as
// connection-local state, and every websocket request still sends `store=false`.
pub(super) fn is_websocket_active_response_error(err: &LLMError) -> bool {
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

pub(super) fn is_websocket_reconnect_error(err: &LLMError) -> bool {
    matches!(err, LLMError::Network { .. })
}

pub(super) fn is_websocket_auth_retryable(error: &tokio_tungstenite::tungstenite::Error) -> bool {
    let message = error.to_string();
    WEBSOCKET_AUTH_RETRY_STATUSES.iter().any(|status| message.contains(status))
}

#[cold]
pub(super) fn format_provider_error(message: String) -> LLMError {
    LLMError::Provider {
        message: error_display::format_llm_error("OpenAI", &message),
        metadata: None,
    }
}

#[cold]
pub(super) fn format_network_error(message: String) -> LLMError {
    LLMError::Network {
        message: error_display::format_llm_error("OpenAI", &message),
        metadata: None,
    }
}
