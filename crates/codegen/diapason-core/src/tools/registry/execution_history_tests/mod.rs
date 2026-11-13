//! Execution-history tests grouped by replay and invalidation concern.

use super::*;
use proptest::prelude::*;
use serde_json::json;
use tempfile::tempdir;

fn make_snapshot() -> HarnessContextSnapshot {
    HarnessContextSnapshot::new("session_test".to_string(), None)
}

fn make_task_snapshot(task_id: &str) -> HarnessContextSnapshot {
    HarnessContextSnapshot::new("session_test".to_string(), Some(task_id.to_string()))
}

/// Record a successful code search and report whether a probe replays it.
fn replays_code_search(cached_args: Value, query_args: &Value) -> bool {
    let history = ToolExecutionHistory::new(10);
    let cached_result = json!({"results": ["cached search"]});
    history.add_record(ToolExecutionRecord::success(
        String::from(tools::CODE_SEARCH),
        String::from(tools::CODE_SEARCH),
        false,
        None,
        cached_args,
        cached_result.clone(),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    history.find_recent_successful_by_read_target(tools::CODE_SEARCH, query_args, Duration::from_secs(60))
        == Some(cached_result)
}

mod code_search_identity;
mod code_search_invalidation;
mod read_target_matching;
mod spool_progress;
mod spooled_results;
