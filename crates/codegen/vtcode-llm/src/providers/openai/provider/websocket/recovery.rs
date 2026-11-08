//! Recovery telemetry and replay prohibition for the Responses WebSocket transport.

use crate::provider::LLMError;
use crate::providers::shared::record_responses_replay_prohibited;

use super::error::responses_output_started_error;

/// Narrow telemetry port for WebSocket recovery decisions.
///
/// The transport reports through this boundary so the process-wide counters
/// stay in [`responses_observability`](crate::providers::shared) and tests can
/// record into a local sink instead of mutating global state.
pub(super) trait WebSocketRecoveryTelemetry {
    /// Record that a post-output failure prohibited automatic replay.
    fn replay_prohibited(&self);
}

/// Production telemetry: the process-wide Responses recovery counters.
pub(super) struct ProcessWebSocketRecoveryTelemetry;

impl WebSocketRecoveryTelemetry for ProcessWebSocketRecoveryTelemetry {
    fn replay_prohibited(&self) {
        record_responses_replay_prohibited();
    }
}

/// Record and convert a post-output failure that must not be replayed.
pub(super) fn prohibit_websocket_replay(telemetry: &impl WebSocketRecoveryTelemetry, error: LLMError) -> LLMError {
    telemetry.replay_prohibited();
    tracing::warn!(
        target = "vtcode::llm::responses::websocket",
        outcome = "replay_prohibited",
        error_category = "responses_websocket_output_started",
        "Responses WebSocket failed after output began; automatic replay is disabled"
    );
    responses_output_started_error(error)
}
