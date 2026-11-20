//! Exercises a launch whose record could not be saved and is then reused.

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
use crate::config::DiapasonConfig;
#[cfg(unix)]
use crate::subagents::background::{background_state_path_for_tests, lock_background_state};
#[cfg(unix)]
use crate::subagents::{BackgroundSubprocessStatus, SpawnBackgroundSubprocessRequest};
#[cfg(unix)]
use crate::tools::types::DiapasonSessionLifecycleState;

#[cfg(unix)]
use super::super::controller_background_settlement::pending_background_ledger_write_generation;
#[cfg(unix)]
use super::support::write_sleeping_demo_script;
#[cfg(unix)]
use super::support::{background_test_controller, write_background_demo_agent, write_ledger};

/// Builds the same launch request for both attempts.
#[cfg(unix)]
fn demo_request() -> SpawnBackgroundSubprocessRequest {
    SpawnBackgroundSubprocessRequest {
        agent_type: Some("background-demo".to_string()),
        message: Some("Report readiness once.".to_string()),
        items: Vec::new(),
        model: None,
        reasoning_effort: None,
        max_turns: None,
    }
}

/// Blocks the ledger's inter-process lock so the next save cannot run.
#[cfg(unix)]
fn block_ledger_lock(workspace_root: &std::path::Path) -> std::path::PathBuf {
    let lock_path = background_state_path_for_tests(workspace_root).with_file_name("background_subagents.lock");
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks the save");
    lock_path
}

/// Reuse must not report a record whose save failed as persisted.
///
/// The launch starts a process, commits the record in memory, and then fails to
/// write the ledger. The caller is told the launch failed, but the process is
/// live: the design keeps the process and owes the write, so the failed save
/// must not be mistaken for a failed launch. A second launch would otherwise
/// reuse that record as if a restart or another process could see it; refresh
/// runs first and must retry the write before reuse returns. The failed save
/// must also leave records the ledger already held untouched.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_whose_save_failed_is_persisted_before_reuse_returns_it() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    // A record persisted before the lock was taken must survive both the
    // failed save and the retried one: neither may truncate the ledger.
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-already-persisted", false, "stopped");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let lock_path = block_ledger_lock(temp.path());
    controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect_err("the launch cannot save its record while the lock path is blocked");

    // The row is committed in memory even though its write failed, so the
    // spawned session must still be running: only the ledger obligation failed.
    let spawned_exec_session_id = {
        let state = controller.state.read().await;
        let record = state
            .background_children
            .get("background-background-demo")
            .expect("the failed save leaves the committed record");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Running,
            "the failed save must not settle or discard the launch it started"
        );
        record.exec_session_id.clone()
    };
    let snapshot = controller
        .config
        .exec_sessions
        .snapshot_session(&spawned_exec_session_id)
        .await
        .expect("the spawned exec session survives a failed ledger save");
    assert_eq!(
        snapshot.lifecycle_state,
        Some(DiapasonSessionLifecycleState::Running),
        "the design keeps the process and owes the write, so the session is still running"
    );

    let ledger_path = background_state_path_for_tests(temp.path());
    let after_failure = std::fs::read_to_string(&ledger_path).expect("the seeded ledger is still readable");
    let ledger: serde_json::Value = serde_json::from_str(&after_failure).expect("the ledger is valid JSON");
    let records = ledger["records"].as_array().expect("records are an array");
    assert!(
        records
            .iter()
            .any(|record| record["id"] == "background-worker" && record["exec_session_id"] == "exec-already-persisted"),
        "a failed save must not remove a record the ledger already held"
    );
    assert!(
        !records.iter().any(|record| record["id"] == "background-background-demo"),
        "the failed save must not claim the launch reached the ledger"
    );

    std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

    let entry = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect("reuse retries the owed write and reports the live subprocess");

    assert_eq!(entry.id, "background-background-demo");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);
    assert_eq!(
        entry.exec_session_id, spawned_exec_session_id,
        "reuse answers for the session the failed launch already started"
    );

    assert!(ledger_path.is_file(), "the record must reach the ledger before reuse reports it");
    let stored = std::fs::read_to_string(&ledger_path).expect("the retried save writes the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let records = ledger["records"].as_array().expect("records are an array");
    assert!(
        records.iter().any(|record| record["id"] == "background-background-demo"),
        "the reused record is the one the ledger now holds"
    );
    assert!(
        records
            .iter()
            .any(|record| record["id"] == "background-worker" && record["exec_session_id"] == "exec-already-persisted"),
        "the retried save merges the record that was already persisted"
    );

    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}

/// Reuse must not answer from memory while the launch's write is in flight.
///
/// The launch commits its record in memory and then waits for the ledger lock.
/// The obligation has to be raised before that wait: raised only after the
/// save fails, the whole window is unmarked, and a concurrent identical
/// request can refresh, see no pending obligation and no persisted record, and
/// return the in-memory `Running` record as `Reused` before durability is
/// known. The test holds the ledger lock to widen that window, then attempts
/// reuse inside it.
#[cfg(unix)]
#[tokio::test]
async fn a_reuse_in_the_save_window_waits_for_the_owed_write() {
    use std::time::Duration as StdDuration;

    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let ledger_guard = lock_background_state(temp.path())
        .await
        .expect("the test holds the ledger lock");

    let first = controller.spawn_background_subprocess(demo_request());
    tokio::pin!(first);
    let mut first_result = None;
    for _ in 0..500 {
        tokio::select! {
            result = &mut first, if first_result.is_none() => first_result = Some(result),
            () = tokio::time::sleep(StdDuration::from_millis(10)) => {}
        }
        if pending_background_ledger_write_generation(&controller.state).is_some() {
            break;
        }
        if first_result.is_some() {
            break;
        }
    }

    assert!(first_result.is_none(), "the launch must still be waiting for the ledger lock, not settled");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the obligation must be recorded before the save is awaited, not only after it fails"
    );

    let reuse = tokio::time::timeout(
        Duration::from_millis(300),
        Box::pin(controller.spawn_background_subprocess(demo_request())),
    )
    .await;
    assert!(reuse.is_err(), "reuse must wait for the owed write rather than report the unsaved record as Reused");

    drop(ledger_guard);

    let first_entry = first.await.expect("the launch saves once the ledger lock is released");
    assert_eq!(first_entry.status, BackgroundSubprocessStatus::Running);
    let reuse_entry = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect("reuse succeeds once the record is durable");
    assert_eq!(reuse_entry.id, first_entry.id, "reuse answers for the same record");
    assert_eq!(reuse_entry.exec_session_id, first_entry.exec_session_id);

    let _ = controller
        .config
        .exec_sessions
        .close_session(&reuse_entry.exec_session_id)
        .await;
    controller.signal_shutdown().await;
}
