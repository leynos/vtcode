//! Pins the record state a retryable close fault leaves behind.
//!
//! Declared by `tests.rs`. The case turns on what a reverted stop must not keep
//! from the terminal state it is leaving.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;

use super::super::controller_background_cancel::fail_next_background_close;
use super::support::{background_record, background_test_controller};

/// A retryable close fault clears the end instant it reverts.
///
/// A retryable close fault rewinds a `Stopped` record to `Running` so the
/// pending stop stays visible. The record's end instant belongs to the settled
/// state and must go with it: the projection maps `ended_at` without checking
/// status, and the ACP contract reserves an end instant for terminal tasks. A
/// task that is active again must not list as running with an end.
#[tokio::test]
async fn a_retryable_close_fault_clears_the_end_instant_it_reverts() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let exec_session_id = "exec-reverted-stop";
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        exec_session_id,
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    // The adopted stop settled the record before its close was attempted.
    record.ended_at = Some(chrono::Utc::now());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    fail_next_background_close(&controller.state, exec_session_id, "close transport fault");

    let confirmed = controller
        .attempt_background_stop_close("background-worker", exec_session_id)
        .await;
    assert!(!confirmed, "a retryable fault leaves the close unconfirmed");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Running,
        "a retryable fault reports the pending stop as still running"
    );
    assert!(
        record.ended_at.is_none(),
        "an active record must not carry the end instant of the stop it reverted: {:?}",
        record.ended_at
    );
    assert_eq!(record.restart_attempts, 1, "the retry allowance stays spent");
    assert!(
        record
            .error
            .as_deref()
            .is_some_and(|error| error.contains("close transport fault")),
        "the fault stays visible: {:?}",
        record.error
    );
    drop(state);

    controller.signal_shutdown().await;
}
