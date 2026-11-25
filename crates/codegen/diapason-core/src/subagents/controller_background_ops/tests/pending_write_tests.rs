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
use super::super::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
};
#[cfg(unix)]
use super::support::{
    background_test_controller, write_background_demo_agent, write_ledger, write_sleeping_demo_script,
};

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

/// Blocks the ledger's inter-process lock so the next lock attempt cannot run.
///
/// A directory at the sidecar's path makes the acquisition fail outright, which
/// is the same refusal a contended ledger eventually reports. It is left in
/// place until the test removes it.
#[cfg(unix)]
fn block_ledger_lock(workspace_root: &std::path::Path) -> std::path::PathBuf {
    let lock_path = background_state_path_for_tests(workspace_root).with_file_name("background_subagents.lock");
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks the save");
    lock_path
}

/// A launch whose ledger cannot be read is refused rather than kept in memory.
///
/// A claim is one durable transition with the row it reconciles, so the
/// reservation takes the ledger lock before it decides anything. A ledger that
/// cannot be locked is therefore a fault the caller sees: nothing is committed,
/// no process is spawned, and no obligation is left behind for a later pass. The
/// failure must also leave the records the ledger already held untouched, and a
/// retry after the contention clears must launch normally.
///
/// This replaces an older fixture that blocked only the final save, which the
/// reservation now reaches after locking the ledger a second time; a record
/// committed in memory and merely owed its write is no longer a state a launch
/// can reach this way.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_whose_ledger_cannot_be_locked_is_refused_before_it_commits() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    // A record persisted before the lock was taken must survive the refusal.
    write_ledger(temp.path(), "background-worker", None, "worker", "exec-already-persisted", false, "stopped");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let lock_path = block_ledger_lock(temp.path());
    let error = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect_err("the launch cannot read the ledger while the lock path is blocked");
    assert!(
        format!("{error:#}").contains("background_subagents.lock"),
        "the refusal names the ledger lock it could not take: {error:#}"
    );

    // The reservation failed before the claim was written, so the record was
    // never created and no obligation is owed.
    {
        let state = controller.state.read().await;
        assert!(
            !state.background_children.contains_key("background-background-demo"),
            "a launch that never read the ledger must not claim the record"
        );
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "a refused launch must not leave a write owed"
    );

    let ledger_path = background_state_path_for_tests(temp.path());
    let after_failure = std::fs::read_to_string(&ledger_path).expect("the seeded ledger is still readable");
    let ledger: serde_json::Value = serde_json::from_str(&after_failure).expect("the ledger is valid JSON");
    let records = ledger["records"].as_array().expect("records are an array");
    assert_eq!(records.len(), 1, "a refused launch must not write a second row");
    assert!(
        records
            .iter()
            .any(|record| record["id"] == "background-worker" && record["exec_session_id"] == "exec-already-persisted"),
        "a refused launch must not remove a record the ledger already held"
    );
    assert!(
        !records.iter().any(|record| record["id"] == "background-background-demo"),
        "the refused launch must not claim it reached the ledger"
    );

    std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

    let entry = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect("the retry launches once the ledger is readable");

    assert_eq!(entry.id, "background-background-demo");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);

    assert!(ledger_path.is_file(), "the launch must reach the ledger before it is reported");
    let stored = std::fs::read_to_string(&ledger_path).expect("the launch writes the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let records = ledger["records"].as_array().expect("records are an array");
    let stored_row = records
        .iter()
        .find(|record| record["id"].as_str() == Some("background-background-demo"))
        .expect("the launched record is the one the ledger now holds");
    assert_eq!(
        stored_row["exec_session_id"],
        entry.exec_session_id.as_str(),
        "the ledger row must name the session the launch started"
    );
    assert_eq!(
        stored_row["status"], "running",
        "the launched record must be persisted as the active launch it reports"
    );
    assert!(
        records
            .iter()
            .any(|record| record["id"] == "background-worker" && record["exec_session_id"] == "exec-already-persisted"),
        "the launch merges the record that was already persisted"
    );

    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}

/// Reuse must not answer from memory while the active record's write is owed.
///
/// A launch can commit its record and then fail to save it, and the record then
/// exists only in this process. The obligation is raised under the lock that
/// makes the record reusable, so a concurrent identical request can never see
/// `Running` with no marker and no persisted row and answer `Reused` before
/// durability is known. The marker a failed save leaves is installed directly
/// here, because the reservation now refuses a launch whose ledger cannot be
/// locked at all and so cannot produce that state by blocking the lock; the
/// save then parks at the ledger checkpoint. The request future is awaited
/// directly, so an unelapsed timeout says nothing about where it waited, and
/// the record it returns must be the durable one.
#[cfg(unix)]
#[tokio::test]
async fn a_reuse_in_the_save_window_waits_for_the_owed_write() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let launched = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect("the launch starts against a readable ledger");
    let launched_exec_session_id = launched.exec_session_id.clone();
    assert_eq!(launched.status, BackgroundSubprocessStatus::Running);

    // The marker a failed save leaves, installed without failing the save: the
    // record is live and wanted, and its write is owed.
    mark_background_ledger_write_pending(&controller.state);
    let (write_entered, write_release) = ledger_write_checkpoints::arm(temp.path());
    let reuse_controller = controller.clone();
    let mut reuse = tokio::spawn(async move { reuse_controller.spawn_background_subprocess(demo_request()).await });

    // The reuse request's owed-write retry is parked at the ledger checkpoint.
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
        reuse_entry.exec_session_id, launched_exec_session_id,
        "reuse answers for the launch that is already running"
    );
    assert_eq!(reuse_entry.status, BackgroundSubprocessStatus::Running);
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        None,
        "the owed write is discharged once reuse has waited for it"
    );

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
