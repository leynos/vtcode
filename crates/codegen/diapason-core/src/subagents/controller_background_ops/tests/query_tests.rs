//! Exercises the read-only snapshot query over a faulting output reader.

use std::sync::Arc;

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, SubagentController, TaskOwner};

use super::support::{background_record, test_controller_config, write_ledger};
use super::{FakeOutput, FakeOutputReader};

/// A snapshot read fault must not settle, rewrite or persist the record.
///
/// `background_snapshot` is the query a management list uses to inspect one
/// task. It is documented as a pure read over cached state, so a storage fault
/// in the preview must reach the caller without the record or the ledger
/// changing: a caller that retries, or a second observer, must see exactly the
/// state the refresh path last committed. The fake reader returns an archive
/// fault, which is a real storage failure rather than an absent task.
#[tokio::test]
async fn a_snapshot_read_fault_leaves_the_record_and_ledger_unchanged() {
    let temp = TempDir::new().expect("tempdir");
    let ledger = write_ledger(temp.path(), "background-worker", None, "worker", "exec-launch", true, "running");
    let persisted_before = std::fs::read(&ledger).expect("ledger is readable");

    let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
    config.output_reader = Some(Arc::new(FakeOutputReader::for_task(
        "background-worker",
        FakeOutput::ArchiveFailure("the session archive is unreadable"),
    )));
    let controller = SubagentController::new(config).await.expect("background controller");

    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    let cached_before = record.clone();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let error = controller
        .background_snapshot(TaskOwner::Workspace, "background-worker")
        .await
        .expect_err("a preview storage fault reaches the caller");

    assert!(
        format!("{error:#}").contains("read background output for background-worker"),
        "the fault is reported against the requested target: {error:#}"
    );

    let state = controller.state.read().await;
    let cached = state
        .background_children
        .get("background-worker")
        .expect("the record is still cached");
    assert_eq!(cached.status, cached_before.status, "the query must not settle the record");
    assert_eq!(cached.desired_enabled, cached_before.desired_enabled);
    assert_eq!(cached.restart_attempts, cached_before.restart_attempts);
    assert_eq!(cached.updated_at, cached_before.updated_at, "the query must not touch the record's instants");
    assert_eq!(cached.ended_at, cached_before.ended_at);
    assert_eq!(cached.error, cached_before.error);
    drop(state);

    let persisted_after = std::fs::read(&ledger).expect("ledger is still readable");
    assert_eq!(persisted_after, persisted_before, "a read-only query must not rewrite the persisted ledger");

    controller.signal_shutdown().await;
}
