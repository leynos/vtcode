//! Classifies what a read of a background record's exec session established.

/// What one read of a background record's exec session established.
///
/// [`Self::Missing`] and [`Self::Unobserved`] lead to opposite actions, so the
/// distinction is the whole point of the type: an exit is evidence that the
/// process ended, while a read fault is only the absence of evidence, and only
/// the former may settle or restart a record.
pub(super) enum BackgroundExecSessionObservation {
    /// The record names no session, or the backend reports that it is gone.
    Missing,
    /// The backend returned the session the record names.
    Live(crate::tools::types::DiapasonExecSession),
    /// The backend could not be read, so nothing is known about the process.
    Unobserved,
}

/// Re-exports the exec-session missing classifier under this module's name.
///
/// The predicate is defined once in `tools::exec_session`, next to the typed
/// cause it classifies, so the liveness projection and the background-output
/// adapter cannot drift apart on what counts as an exit. The alias keeps the
/// `exec_session_is_missing` import path stable for the cancellation,
/// lifecycle, query and test-support modules that already use it.
pub(super) use crate::tools::exec_session::is_missing_exec_session as exec_session_is_missing;

/// Renders a bounded, non-sensitive reason for an unobserved entry.
///
/// Only the outermost error text is used, which is the operation that failed
/// rather than its source chain, and it is truncated to a fixed character
/// budget. The session id is replaced first: a storage backend quotes the
/// record it was asked for in its own message, and this reason reaches a debug
/// log next to a task id, so the identifier must not travel with it. Redaction
/// precedes truncation so a long id cannot be cut in half and survive.
pub(super) fn bounded_liveness_reason(error: &anyhow::Error, session_id: &str) -> String {
    const MAX_REASON_CHARS: usize = 160;
    const REDACTED_SESSION: &str = "<session>";
    let rendered = error.to_string();
    let rendered = if session_id.is_empty() {
        rendered
    } else {
        rendered.replace(session_id, REDACTED_SESSION)
    };
    if rendered.chars().count() <= MAX_REASON_CHARS {
        rendered
    } else {
        rendered.chars().take(MAX_REASON_CHARS).collect()
    }
}
