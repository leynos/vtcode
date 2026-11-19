//! Exercises launch settlement, launch identity, and archive attachment.

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use tempfile::TempDir;

#[cfg(unix)]
use super::super::BackgroundRunRequest;
use super::super::controller_background_run::{attach_background_archive_paths, background_exec_session_id};
#[cfg(unix)]
use super::support::write_background_demo_agent;
use super::support::{background_record, background_test_controller};

/// A launch that a cancellation overtook must not revive the record.
#[tokio::test]
async fn a_launch_does_not_revive_a_record_that_stopped_wanting_it() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Starting,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-launch", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(!committed, "a cancelled record must not be written over by the launch it cancelled");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.pid, None, "the declined launch's process is not the record's process");

    drop(state);

    controller.signal_shutdown().await;
}

/// A launch answers only for the exec session it created.
#[tokio::test]
async fn a_launch_does_not_settle_a_record_that_names_another_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-replacement",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-launch", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(!committed, "a replaced launch must not settle the replacement");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.exec_session_id, "exec-replacement");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);

    drop(state);

    controller.signal_shutdown().await;
}

/// A desired record is committed with the launch's process metadata.
#[tokio::test]
async fn a_launch_commits_to_its_own_desired_record() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    record.ended_at = Some(chrono::Utc::now());
    record.error = Some("previous failure".to_string());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-launch", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(committed, "the launch owns this record and must settle it");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Running);
    assert_eq!(record.pid, Some(7));
    assert_eq!(record.ended_at, None, "an active record must not carry the previous run's end instant");
    assert_eq!(record.error, None, "a running record must not carry the previous run's failure");

    drop(state);

    controller.signal_shutdown().await;
}

/// Two records may not share one exec-session id.
#[tokio::test]
async fn a_launch_cannot_share_an_exec_session_with_another_record() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let other =
        background_record("background-other", None, "other", "exec-shared", BackgroundSubprocessStatus::Running, true);
    let candidate = background_record(
        "background-worker",
        None,
        "worker",
        "exec-shared",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(other.id.clone(), other);
        state.background_children.insert(candidate.id.clone(), candidate);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-shared", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(!committed, "a launch must not adopt an exec session another record already holds");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.pid, None);

    drop(state);

    controller.signal_shutdown().await;
}

/// Two launches of one record in one process never reuse an exec-session id.
#[test]
fn every_generated_exec_session_id_is_unique() {
    let first = background_exec_session_id("session-background-worker");
    let second = background_exec_session_id("session-background-worker");

    assert_ne!(first, second, "a repeated id would make one launch fail on the duplicate-session check");
    assert!(first.starts_with("exec-session-background-worker-"), "{first}");
}

/// Archive paths attach only to the launch that named the resolved session.
#[tokio::test]
async fn archive_paths_attach_only_to_the_launch_that_named_the_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    let archive = temp.path().join("archive.json");

    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-replaced", &archive);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert!(
            record.archive_path.is_none(),
            "an archive resolved for a replaced launch must not attach to the replacement"
        );
        assert!(record.transcript_path.is_none());
    }

    let session_id = {
        let state = controller.state.read().await;
        state
            .background_children
            .get("background-worker")
            .expect("record")
            .session_id
            .clone()
    };
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", &session_id, &archive);
    }
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.archive_path, Some(archive.clone()));
    assert_eq!(record.transcript_path, Some(archive.clone()));
    drop(state);

    controller.signal_shutdown().await;
}

/// A launch that fails twice settles `Error` instead of staying `Starting`.
///
/// The first failure leaves the record `Starting` because the single restart
/// allowance remains, and nothing else drives that state. The launch itself
/// has to consume the allowance and retry; if the retry fails too, the record
/// must become a visible failure rather than an active process forever.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_launch_settles_after_the_retry_is_spent() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("the launch script is unavailable");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.auto_restore = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: None,
            restart_attempts: 0,
            overrides: None,
        })
        .await
        .expect_err("a missing launch script fails the spawn");
    assert!(
        format!("{error:#}").contains("Failed to spawn background subprocess"),
        "the caller receives the spawn error: {error:#}"
    );

    let entry = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.agent_name == "background-demo")
        .expect("the failed launch leaves a record");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "a spent restart allowance must settle the record as a visible failure"
    );
    assert!(entry.ended_at.is_some(), "a failed launch has ended");
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Failed to start background subagent")),
        "{:?}",
        entry.error
    );

    controller.signal_shutdown().await;
}
