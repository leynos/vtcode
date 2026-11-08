//! Recovery telemetry and replay prohibition for the Responses WebSocket transport.

use crate::provider::LLMError;
use crate::providers::shared::{ResponsesRecoveryObservation, ResponsesRecoveryTelemetry, responses_error_category};

use super::error::responses_output_started_error;

/// Record and convert a post-output failure that must not be replayed.
pub(super) fn prohibit_websocket_replay(telemetry: &impl ResponsesRecoveryTelemetry, error: LLMError) -> LLMError {
    telemetry.record(ResponsesRecoveryObservation::ReplayProhibited);
    tracing::warn!(
        target = "vtcode::llm::responses::websocket",
        outcome = "replay_prohibited",
        error_category = "responses_websocket_output_started",
        "Responses WebSocket failed after output began; automatic replay is disabled"
    );
    responses_output_started_error(error)
}

/// Record and log a WebSocket exchange that retries over HTTP.
///
/// The record carries the model, a fixed outcome label and a closed error
/// category. The provider error's message is deliberately omitted: it can hold
/// arbitrary response text, and fallback is normal enough that it must not leak
/// payload detail into logs.
pub(crate) fn record_websocket_http_fallback(
    telemetry: &impl ResponsesRecoveryTelemetry,
    model: &str,
    error: &LLMError,
) {
    telemetry.record(ResponsesRecoveryObservation::HttpFallback);
    tracing::debug!(
        model = %model,
        outcome = "responses_websocket_http_fallback",
        error_category = responses_error_category(error),
        "Responses WebSocket exchange failed; falling back to HTTP"
    );
}
