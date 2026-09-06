//! Tool-output handler tests grouped by output-processing concern.

use super::*;
use std::io::{IsTerminal, stdin};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::{RwLock, mpsc::unbounded_channel};
use vtcode_core::acp::ToolPermissionCache;
use vtcode_core::config::loader::VTCodeConfig;
use vtcode_core::core::decision_tracker::DecisionTracker;
use vtcode_core::core::trajectory::TrajectoryLogger;
use vtcode_core::tools::ApprovalRecorder;
use vtcode_core::tools::registry::{ToolExecutionError, ToolRegistry};
use vtcode_core::tools::result_cache::{ToolCacheKey, ToolResultCache};
use vtcode_core::ui::inline_theme_from_core_styles;
use vtcode_core::ui::theme;
use vtcode_ui::tui::app::{InlineCommand, InlineHandle, SessionOptions, spawn_session_with_options};

fn build_harness_state() -> crate::agent::runloop::unified::run_loop_context::HarnessTurnState {
    crate::agent::runloop::unified::run_loop_context::HarnessTurnState::new(
        crate::agent::runloop::unified::run_loop_context::TurnRunId("test-run".to_string()),
        crate::agent::runloop::unified::run_loop_context::TurnId("test-turn".to_string()),
        4,
        60,
        0,
    )
}

fn dummy_handle() -> InlineHandle {
    InlineHandle::new_for_tests(unbounded_channel().0)
}

mod compact_completion;
mod grouping_boundaries;
mod pipeline_integration;
mod spool_capture;
mod stream_normalization;
mod stream_visibility;
mod task_tracker_and_status;
