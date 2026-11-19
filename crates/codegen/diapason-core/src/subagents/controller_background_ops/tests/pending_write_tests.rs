//! Exercises a launch whose record could not be saved and is then reused.

#[cfg(unix)]
use tempfile::TempDir;

#[cfg(unix)]
use crate::config::DiapasonConfig;
#[cfg(unix)]
use crate::subagents::background::background_state_path_for_tests;
#[cfg(unix)]
use crate::subagents::{BackgroundSubprocessStatus, SpawnBackgroundSubprocessRequest};

#[cfg(unix)]
use super::support::write_sleeping_demo_script;
#[cfg(unix)]
use super::support::{background_test_controller, write_background_demo_agent};

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
/// live. A second launch would otherwise reuse that record as if a restart or
/// another process could see it; refresh runs first and must retry the write
/// before reuse returns.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_whose_save_failed_is_persisted_before_reuse_returns_it() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let lock_path = block_ledger_lock(temp.path());
    controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect_err("the launch cannot save its record while the lock path is blocked");
    std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

    let entry = controller
        .spawn_background_subprocess(demo_request())
        .await
        .expect("reuse retries the owed write and reports the live subprocess");

    assert_eq!(entry.id, "background-background-demo");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);

    let ledger_path = background_state_path_for_tests(temp.path());
    assert!(ledger_path.is_file(), "the record must reach the ledger before reuse reports it");
    let stored = std::fs::read_to_string(&ledger_path).expect("the retried save writes the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    assert!(
        ledger["records"]
            .as_array()
            .expect("records are an array")
            .iter()
            .any(|record| record["id"] == "background-background-demo"),
        "the reused record is the one the ledger now holds"
    );

    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}
