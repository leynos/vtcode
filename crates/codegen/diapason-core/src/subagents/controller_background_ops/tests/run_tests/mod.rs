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
mod identity_tests;
mod ownership_tests;
mod settlement_tests;
