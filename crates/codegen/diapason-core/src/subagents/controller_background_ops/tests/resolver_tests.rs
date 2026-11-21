//! Exercises legacy-id resolution across the background query surface.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::background::background_state_path_for_tests;
use crate::subagents::{BackgroundSubprocessStatus, SubagentController, TaskOwner};

use super::support::{background_test_controller, test_controller_config, write_ledger};

/// Builds a controller scoped to the session that owns the fixture record.
async fn scoped_controller(temp: &TempDir) -> SubagentController {
    SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()),
        Some("session-viewer"),
    )
    .await
    .expect("background controller")
}

/// A legacy id addresses the record persisted under its canonical id.
///
/// The load path rekeys every entry to the owner-scoped id, while a client
/// still holds the workspace-global id an older release wrote. The read has to
/// resolve the old id rather than report the record as unknown.
#[tokio::test]
async fn a_legacy_record_id_resolves_to_the_canonical_record() {
    let temp = TempDir::new().expect("tempdir");
    write_ledger(temp.path(), "background-worker", Some("session-viewer"), "worker", "exec-legacy", true, "running");
    let controller = scoped_controller(&temp).await;

    let entry = controller
        .background_status_for("background-worker")
        .await
        .expect("a legacy id addresses the canonical record");

    assert_eq!(entry.id, "background-worker@session-viewer");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);

    controller.signal_shutdown().await;
}

/// Cancellation by a legacy id settles the canonical record on disk.
#[tokio::test]
async fn a_legacy_record_id_cancels_the_canonical_record() {
    let temp = TempDir::new().expect("tempdir");
    write_ledger(temp.path(), "background-worker", Some("session-viewer"), "worker", "exec-legacy", true, "running");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let entry = controller
        .force_cancel_background(TaskOwner::Workspace, "background-worker")
        .await
        .expect("a legacy id resolves for cancellation")
        .into_entry();

    assert_eq!(entry.id, "background-worker@session-viewer");
    assert!(!entry.desired_enabled);

    let ledger_path = background_state_path_for_tests(temp.path());
    let stored = std::fs::read_to_string(&ledger_path).expect("the cancellation persists the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let record = ledger["records"]
        .as_array()
        .expect("records are an array")
        .iter()
        .find(|record| record["id"] == "background-worker@session-viewer")
        .expect("the stop is written under the canonical id");
    assert_eq!(record["desired_enabled"], serde_json::Value::Bool(false));

    controller.signal_shutdown().await;
}
