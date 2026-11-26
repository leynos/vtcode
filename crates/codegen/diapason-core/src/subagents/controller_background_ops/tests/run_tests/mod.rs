//! Exercises launch settlement, launch identity, and archive attachment.

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, SubagentController, TaskOwner};
use tempfile::TempDir;

use super::super::super::BackgroundLaunchOverrides;
use super::super::BackgroundRunRequest;
use super::super::controller_background_archive::attach_background_archive_paths;
use super::super::controller_background_run::{background_exec_session_id, background_launch_session_id};
#[cfg(unix)]
use super::super::controller_background_run::{gate_next_background_commit, remove_spec_before_next_background_retry};
use super::super::controller_background_settlement::{
    clear_background_ledger_write_pending, pending_background_ledger_write_generation,
};
#[cfg(unix)]
use super::support::RecordingObserver;
#[cfg(unix)]
use super::support::session_is_gone;
#[cfg(unix)]
use super::support::write_background_demo_agent;
#[cfg(unix)]
use super::support::write_sleeping_demo_script;
use super::support::{background_record, background_test_controller, test_controller_config};
use super::{FakeOutput, FakeOutputReader};

mod archive_tests;
mod identity_tests {
    //! Launch and exec-session identity generation.
    //!
    //! Declared by `run_tests/mod.rs`. Pins that every generated launch session id
    //! and exec-session id is unique, which is what lets an archive and a close be
    //! attributed to one launch.
    use super::*;

    /// Two launches of one record in one process never reuse an exec-session id.
    #[test]
    fn every_generated_exec_session_id_is_unique() {
        let first = background_exec_session_id("session-background-worker");
        let second = background_exec_session_id("session-background-worker");

        assert_ne!(first, second, "a repeated id would make one launch fail on the duplicate-session check");
        assert!(first.starts_with("exec-session-background-worker-"), "{first}");
    }

    /// Two launches in one millisecond never share a session id.
    ///
    /// The timestamp component of the session id only has millisecond resolution,
    /// and the archive attachment uses the session id to identify a launch. A
    /// shared id would let the earlier run's archive attach to its replacement, so
    /// the identity must carry a process-unique sequence as well.
    #[test]
    fn every_generated_launch_session_id_is_unique() {
        let base = "parent-session-background-worker-20260101T000000000Z";
        let first = background_launch_session_id(base);
        let second = background_launch_session_id(base);

        assert_ne!(first, second, "two launches in one millisecond must not share a session id");
        assert!(first.starts_with(base), "{first}");
    }
}
mod ownership_tests;
mod settlement_tests;
