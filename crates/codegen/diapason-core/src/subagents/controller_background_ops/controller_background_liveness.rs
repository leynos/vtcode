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

/// Reports whether a liveness read failed because the session is genuinely gone.
///
/// Only a typed not-found cause counts, from either storage layer. The
/// exec-session record is removed before its PTY handle is released, so a read
/// that races the close finds the record and misses the handle; that is the
/// same expected exit as a read that misses the record, and classifying it as
/// a fault would report an unobserved entry for a session that has simply
/// ended. Any other fault — a backend inconsistency or an I/O failure — is a
/// read that told the caller nothing about the process, so the projection must
/// not treat it as an exit.
pub(super) fn exec_session_is_missing(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.is::<crate::tools::exec_session::ExecSessionNotFound>()
            || cause.is::<crate::tools::pty::PtySessionNotFound>()
    })
}

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
