//! Observation boundary for Lody subagent management RPCs.
//!
//! The routing function and the query helpers stay free of telemetry: this
//! decorator owns the `acp_lody_subagent_management` span, the in-flight gauge,
//! the outcome recording and the bounded failure log. It takes the collector as
//! an explicit parameter instead of reading it from the agent, so the boundary
//! can be exercised, and reused, without one.

use std::future::Future;
use std::time::Instant;

use agent_client_protocol::Error as SdkError;
use diapason_core::metrics::MetricsCollector;
use serde_json::Value;
use tracing::{Instrument, field, info_span, warn};

use super::{ManagementOperation, management_error_reason};

/// Serves one management request under its span, gauge guard and failure log.
///
/// All three operations are counted, including the `list` and `output`
/// queries, because the counters describe use of the management surface rather
/// than state mutation.
pub(super) async fn observe_management_request(
    metrics: &MetricsCollector,
    method: &str,
    session_correlation: &str,
    request: impl Future<Output = Result<Value, SdkError>>,
) -> Result<Value, SdkError> {
    let operation = ManagementOperation::of(method);
    let span = info_span!(
        "acp_lody_subagent_management",
        operation = operation.as_str(),
        session_correlation = %session_correlation,
        outcome = field::Empty,
        elapsed_ms = field::Empty,
        error_category = field::Empty
    );
    // The guard owns the in-flight gauge, so a request dropped while it awaits
    // is still recorded as a failure instead of leaving the gauge elevated.
    let management = metrics.begin_acp_management();
    let started_at = Instant::now();
    let result = request.instrument(span.clone()).await;
    let elapsed_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
    let _outcome_recorded = span.record("outcome", if result.is_ok() { "succeeded" } else { "failed" });
    let _elapsed_ms_recorded = span.record("elapsed_ms", elapsed_ms);
    if let Err(error) = &result {
        let error_category = management_error_reason(error);
        let _error_category_recorded = span.record("error_category", error_category);
        // The span has already exited by the time the failure is logged, so
        // the bounded correlation is repeated here rather than inherited.
        warn!(
            operation = operation.as_str(),
            session_correlation = %session_correlation,
            error_category,
            elapsed_ms,
            "Lody subagent management request failed"
        );
    }
    management.finish(result.is_ok());
    result
}
