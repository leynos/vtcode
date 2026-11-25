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
    bounded_reason_text(&error.to_string(), session_id)
}

/// The bounded category a settled launch reports for a failure.
///
/// The record's error reaches a client as the task's `stopReason`, so it names
/// a category and nothing else: any line of the error chain can quote the
/// ledger, lock or workspace path of this process, and truncating a message
/// does not remove a path from it. The full chain still reaches this process's
/// log at the settlement.
///
/// Classification only ever *narrows* what is disclosed. A cause this
/// classification does not recognize falls back to the generic category, so an
/// unrecognized or reworded fault loses detail and can never leak one. The
/// ledger-lock deadline is matched on its typed cause rather than on text.
pub(super) fn launch_failure_category(error: &anyhow::Error) -> &'static str {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::subagents::background_lock::BackgroundStateLockTimeout>()
            .is_some()
    }) {
        return "the shared background ledger lock was not available in time";
    }
    let chain = error.chain().map(|cause| cause.to_string()).collect::<Vec<_>>();
    let mentions = |needle: &str| chain.iter().any(|message| message.contains(needle));
    if mentions("Failed to spawn") || mentions("failed to spawn pipe session") {
        return "the subprocess could not be spawned";
    }
    if mentions("did not keep the claim") {
        return "the background ledger kept another launch under this record";
    }
    if mentions("Failed to parse")
        || mentions("Failed to read")
        || mentions("Failed to open")
        || mentions("Failed to lock")
        || mentions("Failed to persist")
    {
        return "the background ledger could not be updated";
    }
    "the launch could not be completed"
}

/// Redacts the session id from `rendered`, then truncates it to the budget.
fn bounded_reason_text(rendered: &str, session_id: &str) -> String {
    const MAX_REASON_CHARS: usize = 160;
    const REDACTED_SESSION: &str = "<session>";
    let rendered = if session_id.is_empty() {
        rendered.to_owned()
    } else {
        rendered.replace(session_id, REDACTED_SESSION)
    };
    if rendered.chars().count() <= MAX_REASON_CHARS {
        rendered
    } else {
        rendered.chars().take(MAX_REASON_CHARS).collect()
    }
}

#[cfg(test)]
mod failure_category_tests {
    //! Pins that a settled launch failure names no local path.
    //!
    //! `record.error` is copied verbatim into a task's `stopReason`, so the
    //! category is the only text a client receives for a settled launch fault.

    use std::path::Path;
    use std::time::Duration;

    use super::launch_failure_category;

    /// Every category is a bounded sentence that names no local path.
    ///
    /// The ledger-lock deadline is the fault a bound restart hits, and its own
    /// message carries the sidecar path for this process's log. The projected
    /// category must reduce it, and an unclassified message must not be copied
    /// through as a fallback.
    #[test]
    fn a_settled_launch_failure_names_no_local_path() {
        let deadline = crate::subagents::background_lock::lock_timeout_error(
            Path::new("/workspace/.diapason/state/background_subagents.lock"),
            Duration::from_millis(250),
        );
        let category = launch_failure_category(&deadline);
        assert!(!category.contains('/'), "a category must name no path: {category}");
        assert!(!category.contains("background_subagents"), "a category must name no sidecar: {category}");
        assert!(category.contains("lock"), "the deadline keeps its category: {category}");

        let spawn = anyhow::anyhow!("failed to spawn pipe session 'exec-1'")
            .context("Failed to spawn background subprocess for subagent 'worker'");
        let category = launch_failure_category(&spawn);
        assert!(!category.contains('/'), "{category}");
        assert!(category.contains("spawned"), "a spawn fault keeps its category: {category}");

        let generic = anyhow::anyhow!("an unclassified fault naming /secret/path");
        let category = launch_failure_category(&generic);
        assert!(!category.contains('/'), "the fallback must not copy the message it could not classify: {category}");
    }
}
