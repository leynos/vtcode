//! Session-level completion notification helper.
//!
//! Mirrors `turn_loop/notifications.rs` but fires once per whole session at
//! finalization, after the `thread.completed` harness event is emitted. The
//! mapping is a pure function over `ThreadCompletionSubtype` so it can be
//! unit-tested in isolation; delivery is best-effort and never fails the
//! session.

use diapason_core::exec::events::ThreadCompletionSubtype;
use diapason_core::notifications::{CompletionStatus, NotificationEvent, send_global_notification};

/// Map a session completion subtype + turn count to a notification event.
///
/// Exhaustion/execution errors collapse to `Failure` (the user must act),
/// and the forward-compat `Unknown` maps to `PartialSuccess` so the user is
/// alerted to check the result without crying "failure".
pub(super) fn session_completion_notification(subtype: ThreadCompletionSubtype, num_turns: usize) -> NotificationEvent {
    let status = match subtype {
        ThreadCompletionSubtype::Success => CompletionStatus::Success,
        ThreadCompletionSubtype::Cancelled => CompletionStatus::Cancelled,
        ThreadCompletionSubtype::ErrorMaxTurns
        | ThreadCompletionSubtype::ErrorMaxBudgetUsd
        | ThreadCompletionSubtype::ErrorDuringExecution => CompletionStatus::Failure,
        ThreadCompletionSubtype::Unknown => CompletionStatus::PartialSuccess,
    };
    NotificationEvent::Completion {
        task: "session".to_string(),
        status,
        details: Some(format!("{num_turns} turn(s)")),
    }
}

/// Emit the session-completion notification. Best-effort: logs failures at
/// debug level and never propagates them to the caller.
pub(super) async fn emit_session_completion_notification(subtype: ThreadCompletionSubtype, num_turns: usize) {
    let event = session_completion_notification(subtype, num_turns);
    if let Err(err) = send_global_notification(event).await {
        tracing::debug!(error = %err, "Failed to emit session completion notification");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_maps_to_success() {
        let event = session_completion_notification(ThreadCompletionSubtype::Success, 5);
        match event {
            NotificationEvent::Completion { task, status, details } => {
                assert_eq!(task, "session");
                assert_eq!(status, CompletionStatus::Success);
                assert_eq!(details.as_deref(), Some("5 turn(s)"));
            }
            other => panic!("expected Completion, got {other:?}"),
        }
    }

    #[test]
    fn cancelled_maps_to_cancelled() {
        let event = session_completion_notification(ThreadCompletionSubtype::Cancelled, 1);
        match event {
            NotificationEvent::Completion { status, .. } => assert_eq!(status, CompletionStatus::Cancelled),
            other => panic!("expected Completion, got {other:?}"),
        }
    }

    #[test]
    fn execution_errors_map_to_failure() {
        for subtype in [
            ThreadCompletionSubtype::ErrorMaxTurns,
            ThreadCompletionSubtype::ErrorMaxBudgetUsd,
            ThreadCompletionSubtype::ErrorDuringExecution,
        ] {
            let event = session_completion_notification(subtype, 3);
            match event {
                NotificationEvent::Completion { status, .. } => assert_eq!(status, CompletionStatus::Failure),
                other => panic!("expected Completion, got {other:?}"),
            }
        }
    }

    #[test]
    fn unknown_maps_to_partial_success() {
        let event = session_completion_notification(ThreadCompletionSubtype::Unknown, 2);
        match event {
            NotificationEvent::Completion { status, .. } => assert_eq!(status, CompletionStatus::PartialSuccess),
            other => panic!("expected Completion, got {other:?}"),
        }
    }
}
