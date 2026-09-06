//! Spool-progress matching and read-only loop-limit tests.

use super::*;

#[test]
fn finds_recent_read_file_spool_progress() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({"path": ".vtcode/context/tool_outputs/command_session_123.txt"});
    let result = json!({
        "success": true,
        "spool_chunked": true,
        "has_more": true,
        "next_read_args": {
            "path": ".vtcode/context/tool_outputs/command_session_123.txt",
            "offset": 41,
            "limit": 40
        }
    });

    history.add_record(ToolExecutionRecord::success(
        "read_file".to_string(),
        "read_file".to_string(),
        false,
        None,
        args,
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_read_file_spool_progress(
        ".vtcode/context/tool_outputs/command_session_123.txt",
        Duration::from_secs(60),
    );
    assert_eq!(found, Some((41, 40)));
}

#[test]
fn finds_recent_file_operation_read_spool_progress() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({
        "action": "read",
        "path": ".vtcode/context/tool_outputs/command_session_456.txt"
    });
    let result = json!({
        "success": true,
        "spool_chunked": true,
        "has_more": true,
        "next_read_args": {
            "path": ".vtcode/context/tool_outputs/command_session_456.txt",
            "offset": 81,
            "limit": 40
        }
    });

    history.add_record(ToolExecutionRecord::success(
        tools::UNIFIED_FILE.to_string(),
        tools::UNIFIED_FILE.to_string(),
        false,
        None,
        args,
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_read_file_spool_progress(
        ".vtcode/context/tool_outputs/command_session_456.txt",
        Duration::from_secs(60),
    );
    assert_eq!(found, Some((81, 40)));
}

#[test]
fn matches_read_file_alias_name_and_abs_relative_spool_path() {
    let history = ToolExecutionHistory::new(10);
    let rel_path = ".vtcode/context/tool_outputs/command_session_789.txt";
    let abs_path = env::current_dir().unwrap().join(rel_path);
    let args = json!({
        "path": abs_path,
        "offset": 1,
        "limit": 40
    });
    let result = json!({
        "success": true,
        "spool_chunked": true,
        "has_more": true,
        "next_read_args": {
            "path": rel_path,
            "offset": 41,
            "limit": 40
        }
    });

    history.add_record(ToolExecutionRecord::success(
        "Read file".to_string(),
        "Read file".to_string(),
        false,
        None,
        args,
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_read_file_spool_progress(rel_path, Duration::from_secs(60));
    assert_eq!(found, Some((41, 40)));
}

#[test]
fn matches_prefixed_read_file_tool_name() {
    let history = ToolExecutionHistory::new(10);
    let path = ".vtcode/context/tool_outputs/command_session_prefixed.txt";
    let args = json!({ "path": path });
    let result = json!({
        "success": true,
        "spool_chunked": true,
        "has_more": true,
        "next_read_args": {
            "path": path,
            "offset": 121,
            "limit": 40
        }
    });

    history.add_record(ToolExecutionRecord::success(
        "repo_browser.read_file".to_string(),
        "repo_browser.read_file".to_string(),
        false,
        None,
        args,
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_read_file_spool_progress(path, Duration::from_secs(60));
    assert_eq!(found, Some((121, 40)));
}

#[test]
fn ignores_read_file_spool_progress_without_canonical_args() {
    let history = ToolExecutionHistory::new(10);
    let path = ".vtcode/context/tool_outputs/command_session_legacy.txt";
    let args = json!({"path": path});
    let result = json!({
        "success": true,
        "spool_chunked": true,
        "has_more": true,
        "next_offset": 33,
        "chunk_limit": 32
    });

    history.add_record(ToolExecutionRecord::success(
        "read_file".to_string(),
        "read_file".to_string(),
        false,
        None,
        args,
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_read_file_spool_progress(path, Duration::from_secs(60));
    assert_eq!(found, None);
}

#[test]
fn readonly_file_operation_calls_use_lower_identical_limit() {
    let history = ToolExecutionHistory::new(10);
    history.set_loop_detection_limits(5, 2);

    let args = json!({
        "action": "read",
        "path": "crates/codegen/vtcode-core/src/core/agent/runner/tests.rs"
    });

    // The effective limit is max(base_limit, MIN_READONLY_IDENTICAL_LIMIT).
    // With MIN_READONLY_IDENTICAL_LIMIT=2, the limit matches the base.
    assert_eq!(history.loop_limit_for(tools::UNIFIED_FILE, &args), 2);
}
