//! Exercises a launch whose record could not be saved and is then reused.

#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
use crate::config::DiapasonConfig;
#[cfg(unix)]
use crate::subagents::background::background_state_path_for_tests;
#[cfg(unix)]
use crate::subagents::controller_persistence::ledger_write_checkpoints;
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
    let stored_row = records
        .iter()
        .find(|record| record["id"].as_str() == Some("background-background-demo"))
        .expect("the reused record is the one the ledger now holds");
    assert_eq!(
        stored_row["exec_session_id"],
        spawned_exec_session_id.as_str(),
        "the ledger row must name the session the failed launch spawned"
    );
    assert_eq!(
        stored_row["status"], "running",
        "the reused record must be persisted as the active launch it reports"
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

/// Reuse must not answer from memory while the launch's write is owed.
///
/// The launch commits its record in memory and then discovers that the ledger
/// cannot be written. The obligation has to be raised before that save is
/// attempted: raised only after the failure, the whole window is unmarked, and
/// a concurrent identical request can refresh, see no pending obligation and no
/// persisted record, and return the in-memory `Running` record as `Reused`
/// before durability is known. The first launch fails its save by blocking the
/// ledger lock's path, which leaves the process running and the obligation
/// owed. The test then starts the reuse request, whose own retry save parks at
/// the persistence checkpoint: the request future is awaited directly, so an
/// unelapsed timeout says nothing about where it waited. The record it returns
/// must be the durable one.
#[cfg(unix)]
#[tokio::test]
async fn a_reuse_in_the_save_window_waits_for_the_owed_write() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let lock_path = block_ledger_lock(temp.path());
    let error = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect_err("the launch cannot save its record while the lock path is blocked");
    assert!(
        format!("{error:#}").contains("background_subagents.lock"),
        "the launch fails on the ledger lock: {error:#}"
    );
    // The row is committed in memory even though its write failed, so the
    // spawned session is still running and only the ledger obligation failed.
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
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the obligation must be recorded before the save is awaited, not only after it fails"
    );
    std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

    let (write_entered, write_release) = ledger_write_checkpoints::arm(temp.path());
    let reuse_controller = controller.clone();
    let mut reuse = tokio::spawn(async move { reuse_controller.spawn_background_subprocess(demo_request()).await });

    // The reuse request's own retry save is parked at the ledger checkpoint.
    // It may not answer before that write has taken the record to disk.
    tokio::select! {
        result = &mut reuse => panic!("reuse answered before the owed write reached the ledger: {result:?}"),
        () = write_entered.notified() => {}
    }
    assert!(!reuse.is_finished(), "reuse must not answer while the owed write is parked");
    write_release.notify_one();

    let reuse_entry = reuse
        .await
        .expect("the reuse task joins")
        .expect("reuse succeeds once the record is durable");
    assert_eq!(reuse_entry.id, "background-background-demo");
    assert_eq!(
        reuse_entry.exec_session_id, spawned_exec_session_id,
        "reuse answers for the session the failed launch already started"
    );
    assert_eq!(reuse_entry.status, BackgroundSubprocessStatus::Running);

    let ledger_path = background_state_path_for_tests(temp.path());
    let stored = std::fs::read_to_string(&ledger_path).expect("the owed write reaches the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let records = ledger["records"].as_array().expect("records are an array");
    let stored_row = records
        .iter()
        .find(|record| record["id"].as_str() == Some(reuse_entry.id.as_str()))
        .expect("the record reuse reported must be durable");
    assert_eq!(
        stored_row["exec_session_id"],
        reuse_entry.exec_session_id.as_str(),
        "the durable row must name the launch reuse reported"
    );
    assert_eq!(stored_row["status"], "running", "reuse may only answer for an active durable record");

    let _ = controller
        .config
        .exec_sessions
        .close_session(&reuse_entry.exec_session_id)
        .await;
    controller.signal_shutdown().await;
}
