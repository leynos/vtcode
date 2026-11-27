//! Pins that one record's archive fault cannot end a refresh early.
//!
//! Declared by `tests.rs`. The archive lookup reads a file another process
//! writes, so its fault is a per-record condition, not a failure of the pass.

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::subagents::BackgroundSubprocessStatus;
use crate::subagents::background::load_background_state;
use crate::utils::session_archive::override_sessions_dir_for_tests;

use super::super::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
};
use super::support::{background_record, background_test_controller};

/// A session-directory override that is cleared when the test ends, even on panic.
struct SessionsDirOverride;

impl Drop for SessionsDirOverride {
    fn drop(&mut self) {
        crate::utils::session_archive::clear_sessions_dir_override_for_tests();
    }
}

/// One record's unreadable archive must not starve the owed ledger write.
///
/// Refresh reads each record's archive after settling it. That lookup reads a
/// file another process writes, so a malformed archive keeps failing on every
/// pass until someone repairs it. Returning that fault would end the refresh
/// before the owed write it exists to retry, leaving a confirmed stop unsaved
/// for as long as the archive stays broken - across restarts. The fault is
/// reported against its record instead, and the pass reaches its save.
#[tokio::test]
#[serial_test::serial(session_dir_override)]
async fn an_unreadable_archive_does_not_block_the_owed_ledger_write() {
    let temp = TempDir::new().expect("tempdir");
    let sessions_dir = temp.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    // A malformed archive under this record's session id makes the lookup fail
    // every time it runs.
    std::fs::write(sessions_dir.join("session-background-worker.json"), "not json")
        .expect("write the unreadable archive");
    override_sessions_dir_for_tests(&sessions_dir);
    let _override = SessionsDirOverride;

    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-missing",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    mark_background_ledger_write_pending(&controller.state);

    controller
        .refresh_background_processes()
        .await
        .expect("one record's archive fault must not fail the whole refresh");

    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the refresh must reach and discharge the owed write despite the archive fault"
    );
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    assert!(
        persisted.records.iter().any(|stored| stored.id == "background-worker"),
        "the settled record reaches the ledger even while its archive stays unreadable"
    );

    controller.signal_shutdown().await;
}
