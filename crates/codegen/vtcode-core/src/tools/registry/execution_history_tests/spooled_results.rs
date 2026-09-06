//! Spooled-result lookup, telemetry, length, and invalidation tests.

use super::*;

#[test]
fn finds_recent_spooled_result() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({"command": "git diff"});
    let temp = tempdir().unwrap();
    let spool_path = temp.path().join("spooled-output.txt");
    std::fs::write(&spool_path, "diff output").unwrap();
    let result = json!({
        "spool_path": spool_path,
        "success": true
    });

    history.add_record(ToolExecutionRecord::success(
        "run_pty_cmd".to_string(),
        "run_pty_cmd".to_string(),
        false,
        None,
        args.clone(),
        result.clone(),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_spooled_result("run_pty_cmd", &args, Duration::from_secs(60));
    assert_eq!(found, Some(result));
}

#[test]
fn task_telemetry_snapshot_counts_tool_surface_metrics() {
    let history = ToolExecutionHistory::new(10);
    let task = "repo_task_1";
    let command_args = json!({
        "cmd": "rg ToolTaskTelemetrySnapshot vtcode-core/src",
        "sandbox_permissions": "require_escalated",
    });
    let spool_path = "/tmp/vtcode-spool-1.txt";

    history.add_record(ToolExecutionRecord::success(
        tools::UNIFIED_EXEC.to_string(),
        tools::EXEC_COMMAND.to_string(),
        false,
        None,
        command_args.clone(),
        json!({"spool_path": spool_path}),
        make_task_snapshot(task),
        None,
        None,
        None,
        None,
        false,
    ));
    history.add_record(ToolExecutionRecord::success(
        tools::UNIFIED_EXEC.to_string(),
        tools::EXEC_COMMAND.to_string(),
        false,
        None,
        command_args,
        json!({"status": "ok"}),
        make_task_snapshot(task),
        None,
        None,
        None,
        None,
        false,
    ));
    history.add_record(ToolExecutionRecord::success(
        tools::UNIFIED_EXEC.to_string(),
        tools::EXEC_COMMAND.to_string(),
        false,
        None,
        json!({"spool_path": spool_path, "query": "warning"}),
        json!({"spool_path": spool_path, "matches": []}),
        make_task_snapshot(task),
        None,
        None,
        None,
        None,
        false,
    ));
    history.add_record(ToolExecutionRecord::success(
        tools::CODE_SEARCH.to_string(),
        tools::CODE_SEARCH.to_string(),
        false,
        None,
        json!({"query": "ToolRegistry", "result_types": ["definition"]}),
        json!({"query": "ToolRegistry", "filters": {"path": ".", "file_types": [], "result_types": ["definition"], "max_results": 20}, "results": [], "returned": 0, "truncated": false, "hints": []}),
        make_task_snapshot(task),
        None,
        None,
        None,
        None,
        false,
    ));
    history.add_record(ToolExecutionRecord::failure(
        tools::UNIFIED_FILE.to_string(),
        "file_operation".to_string(),
        false,
        None,
        json!({"input": "*** Begin Patch\n*** End Patch\n"}),
        "invalid patch".to_string(),
        make_task_snapshot(task),
        None,
        None,
        None,
        None,
        false,
    ));

    let snapshot = history.task_telemetry_snapshot(Some(task), Some(false));
    assert_eq!(snapshot.total_tool_calls, 5);
    assert_eq!(snapshot.repeated_equivalent_calls, 1);
    assert_eq!(snapshot.failed_tool_calls, 1);
    assert_eq!(snapshot.spooled_outputs, 1);
    assert_eq!(snapshot.fallback_calls, 0);
    assert_eq!(snapshot.read_after_spool_calls, 1);
    assert_eq!(snapshot.command_approval_prompts, 2);
    assert_eq!(snapshot.task_completed_successfully, Some(false));
    assert_eq!(snapshot.calls_by_tool.get(tools::EXEC_COMMAND), Some(&3));
    assert_eq!(snapshot.calls_by_tool.get(tools::CODE_SEARCH), Some(&1));
    assert_eq!(snapshot.calls_by_tool.get("file_operation"), Some(&1));
    assert!(!snapshot.calls_by_tool.keys().any(|label| label.contains("unified_")));

    let json = snapshot.to_json();
    assert_eq!(json["total_tool_calls"], 5);
    assert_eq!(json["task_completed_successfully"], false);
}

#[test]
fn ignores_non_spooled_or_stale_results() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({"path": "README.md"});

    let mut record = ToolExecutionRecord::success(
        "read_file".to_string(),
        "read_file".to_string(),
        false,
        None,
        args.clone(),
        json!({"content": "small"}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    );
    record.timestamp = SystemTime::UNIX_EPOCH;
    history.add_record(record);

    let found = history.find_recent_spooled_result("read_file", &args, Duration::from_secs(60));
    assert!(found.is_none());
}

#[test]
fn ignores_spooled_result_when_spool_file_is_missing() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({"command": "cargo clippy"});
    let missing_spool_path = tempdir().unwrap().path().join("missing_spool.txt");
    let result = json!({
        "spool_path": missing_spool_path,
        "success": true
    });

    history.add_record(ToolExecutionRecord::success(
        "run_pty_cmd".to_string(),
        "run_pty_cmd".to_string(),
        false,
        None,
        args.clone(),
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_spooled_result("run_pty_cmd", &args, Duration::from_secs(60));
    assert!(found.is_none());
}

#[test]
fn find_recent_successful_result_skips_missing_spool_file() {
    let history = ToolExecutionHistory::new(10);
    let args = json!({"command": "cargo clippy"});
    let missing_spool_path = tempdir().unwrap().path().join("missing_spool.txt");
    let result = json!({
        "spool_path": missing_spool_path,
        "success": true
    });

    history.add_record(ToolExecutionRecord::success(
        "run_pty_cmd".to_string(),
        "run_pty_cmd".to_string(),
        false,
        None,
        args.clone(),
        result,
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    let found = history.find_recent_successful_result("run_pty_cmd", &args, Duration::from_secs(60));
    assert!(found.is_none());
}

#[test]
fn len_tracks_records_and_clear() {
    let history = ToolExecutionHistory::new(10);
    assert_eq!(history.len(), 0);
    assert!(history.is_empty());

    history.add_record(ToolExecutionRecord::success(
        "read_file".to_string(),
        "read_file".to_string(),
        false,
        None,
        json!({"path": "README.md"}),
        json!({"success": true}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    assert_eq!(history.len(), 1);
    assert!(!history.is_empty());

    history.clear();
    assert_eq!(history.len(), 0);
    assert!(history.is_empty());
}

#[test]
fn invalidate_all_reads_drops_read_records_only() {
    let history = ToolExecutionHistory::new(10);
    history.add_record(ToolExecutionRecord::success(
        "read_file".to_string(),
        "read_file".to_string(),
        false,
        None,
        json!({"path": "src/main.rs"}),
        json!({"success": true}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));
    history.add_record(ToolExecutionRecord::success(
        "code_search".to_string(),
        "code_search".to_string(),
        false,
        None,
        json!({"query": "fn main"}),
        json!({"success": true}),
        make_snapshot(),
        None,
        None,
        None,
        None,
        false,
    ));

    history.invalidate_all_reads();
    assert_eq!(history.len(), 1, "code_search record must survive");
}
