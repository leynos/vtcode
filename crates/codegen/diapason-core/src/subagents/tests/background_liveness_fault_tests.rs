//! A failed liveness read is not an observed exit.
//!
//! A read that fails for any reason other than the session being gone must
//! leave the record and the ledger untouched, must not respawn work, and must
//! report the fault without carrying the exec-session identifier.
use super::background_liveness_support::*;
use super::*;

/// A liveness read that fails for any reason other than a missing session must
/// not be reported as an exited process.
///
/// "The process exited" and "the read failed" are different facts. The record
/// keeps its cached status, the caller can see the entry was unobserved with a
/// bounded reason, and one bad entry does not drop the rest of the list.
#[tokio::test]
async fn liveness_read_failure_keeps_the_cached_status_and_is_unobserved() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    write_ledger(
        &state_dir.join("background_subagents.json"),
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
            true,
            None,
        )],
    );
    let config = auto_restore_config(temp.path().to_path_buf(), true);
    config.exec_sessions.fail_snapshot_reads("exec session store is unavailable");
    let controller = SubagentController::new_with_background_owner(config, Some("session-a"))
        .await
        .expect("controller");

    let observed = controller.background_status_entries_with_liveness().await;
    assert_eq!(observed.len(), 1, "one unobserved record must not abort the list");
    let entry = observed
        .iter()
        .find(|observed| observed.entry.id == "background-worker@session-a")
        .expect("owned background record");

    assert_eq!(
        entry.entry.status,
        BackgroundSubprocessStatus::Running,
        "a failed liveness read must not fabricate a terminal status"
    );
    assert!(entry.entry.ended_at.is_none(), "an unobserved record keeps its active terminal state");
    match &entry.liveness {
        BackgroundLiveness::Unobserved { reason } => {
            assert!(
                reason.contains("exec session store is unavailable"),
                "the reason must name the failed read, got: {reason}"
            );
            assert!(!reason.contains('/'), "the bounded reason must not carry a path, got: {reason}");
        }
        BackgroundLiveness::Observed => panic!("a failed liveness read must be reported as unobserved"),
    }
}

/// A restore must not respawn a record whose exec session could not be read.
///
/// Restore exists to restart work it believes has exited. A read fault is not
/// an observed exit: treating one as "not running" would start a second
/// process for a task that is still alive, so the record keeps its exec
/// session, its restart budget and its cached status, and the ledger is not
/// rewritten.
#[tokio::test]
async fn restore_does_not_respawn_when_the_exec_session_read_fails() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let ledger_path = state_dir.join("background_subagents.json");
    write_ledger(
        &ledger_path,
        vec![record_with_restart_state(
            "background-worker",
            Some("session-a"),
            "running",
            0,
        )],
    );
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before restore");
    let config = auto_restore_config(temp.path().to_path_buf(), true);
    config.exec_sessions.fail_snapshot_reads("exec session store is unavailable");
    let controller = SubagentController::new_with_background_owner(config, Some("session-a"))
        .await
        .expect("controller");

    let restored = controller
        .restore_background_subagents()
        .await
        .expect("restore reports no error");
    assert_eq!(restored.len(), 1, "restore reports the record it left alone");
    assert_eq!(
        restored[0].status,
        BackgroundSubprocessStatus::Running,
        "an unreadable exec session must not settle or restart the record"
    );
    assert_eq!(
        restored[0].exec_session_id, "exec-background-worker",
        "a failed read must keep the record's own exec session, not start a replacement"
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after restore"),
        ledger_before,
        "an unchanged record must not rewrite the persisted ledger"
    );
}

/// A refresh must leave a record alone when its exec session cannot be read.
///
/// A successful read that reports the session is gone settles the record as
/// `Error`; a failed read knows nothing, so it must neither settle the record
/// nor schedule a replacement launch.
#[tokio::test]
async fn refresh_leaves_the_record_unchanged_when_the_exec_session_read_fails() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let ledger_path = state_dir.join("background_subagents.json");
    write_ledger(
        &ledger_path,
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            0,
            true,
            None,
        )],
    );
    let ledger_before = std::fs::read(&ledger_path).expect("read ledger before refresh");
    let config = auto_restore_config(temp.path().to_path_buf(), true);
    config.exec_sessions.fail_snapshot_reads("exec session store is unavailable");
    let controller = SubagentController::new_with_background_owner(config, Some("session-a"))
        .await
        .expect("controller");

    let refreshed = controller
        .refresh_background_processes()
        .await
        .expect("refresh reports no error");
    assert_eq!(refreshed.len(), 1, "refresh reports the record it left alone");
    assert_eq!(
        refreshed[0].status,
        BackgroundSubprocessStatus::Running,
        "a failed read must not settle the record as an error"
    );
    assert!(
        refreshed[0].ended_at.is_none(),
        "a record that never observed an exit must not gain a terminal instant"
    );
    assert_eq!(
        refreshed[0].exec_session_id, "exec-background-worker",
        "a failed read must keep the record's own exec session, not start a replacement"
    );
    assert_eq!(
        std::fs::read(&ledger_path).expect("read ledger after refresh"),
        ledger_before,
        "an unchanged record must not rewrite the persisted ledger"
    );
}

/// A liveness reason never carries the exec-session identifier.
///
/// A storage backend quotes the record it was asked for in its own message, so
/// the rendered fault can contain the exec-session id. The reason travels to a
/// debug log beside a task id, so the identifier is replaced before the text is
/// bounded; otherwise a log record would spread an id from another record's
/// key space.
#[tokio::test]
async fn liveness_reason_redacts_the_exec_session_identifier() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    write_ledger(
        &state_dir.join("background_subagents.json"),
        vec![record_with_terminal_state(
            "background-worker",
            Some("session-a"),
            "running",
            1,
            true,
            None,
        )],
    );
    let config = auto_restore_config(temp.path().to_path_buf(), true);
    config
        .exec_sessions
        .fail_snapshot_reads("PTY session 'exec-background-worker' not found");
    let controller = SubagentController::new_with_background_owner(config, Some("session-a"))
        .await
        .expect("controller");

    let observed = controller.background_status_entries_with_liveness().await;
    let entry = observed
        .iter()
        .find(|observed| observed.entry.id == "background-worker@session-a")
        .expect("owned background record");
    match &entry.liveness {
        BackgroundLiveness::Unobserved { reason } => {
            assert!(
                !reason.contains("exec-background-worker"),
                "the reason must not carry the exec-session id, got: {reason}"
            );
            assert!(
                reason.contains("<session>"),
                "the redaction must stay visible so the operator knows why, got: {reason}"
            );
        }
        BackgroundLiveness::Observed => panic!("a failed liveness read must be reported as unobserved"),
    }
}
