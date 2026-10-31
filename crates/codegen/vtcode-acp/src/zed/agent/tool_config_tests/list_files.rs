//! Privacy regressions for the ACP `list_files` execution boundary.
use super::support::build_agent;
use crate::zed::agent::test_log_capture::capturing_async;
use crate::zed::constants::TOOL_LIST_FILES_FAILURE;
use assert_fs::TempDir;
use serde_json::json;
use std::sync::Arc;
use tracing_subscriber::filter::LevelFilter;
use vtcode_core::tools::file_ops::FileOpsTool;
use vtcode_core::tools::grep_file::GrepSearchManager;
use vtcode_core::tools::traits::Tool;

/// A tool failure whose originating error embeds caller-supplied text must not
/// leak that text into default diagnostics or the client-facing response.
///
/// The failing mode is a deterministic, filesystem-free `FileOpsTool` failure,
/// so this exercises the real `tool.execute` error branch rather than the
/// reporting helper in isolation.
#[tokio::test]
async fn list_files_tool_failure_keeps_caller_detail_out_of_default_diagnostics() {
    let temp = TempDir::new().expect("temporary workspace");
    let agent = build_agent(temp.path()).await;
    let tool = FileOpsTool::new(temp.path().to_path_buf(), Arc::new(GrepSearchManager::new(temp.path().to_path_buf())));
    let sentinel = "private-sentinel-list-mode";
    let args = json!({"path": ".", "mode": sentinel});

    let tool_error = tool.execute(args.clone()).await.expect_err("the unsupported mode must fail");
    let tool_detail = format!("{tool_error:#}");
    assert!(
        tool_detail.contains(sentinel),
        "precondition: the originating error must carry the sentinel, got {tool_detail:?}"
    );

    let (result, warnings) =
        capturing_async(LevelFilter::INFO, agent.run_list_files_for_tool(Some(&tool), None, &args)).await;

    let message = result.err().expect("a failed listing must surface as a tool failure");
    assert_eq!(message, TOOL_LIST_FILES_FAILURE, "the client-facing failure must be the fixed message");

    let warned = warnings.join("\n");
    assert!(warned.contains("operation=\"list_files\""));
    assert!(warned.contains("component=\"list_files\""));
    assert!(warned.contains("outcome=\"failed\""));
    assert!(warned.contains("error_category=\"listing_failed\""));
    assert!(warned.contains("error_chain_depth="));
    assert!(
        !warned.contains(sentinel),
        "caller-supplied text must stay out of operator-visible logs, got {warned}"
    );
    assert!(
        !warned.contains("Unsupported file operation mode"),
        "raw tool text must stay out of operator-visible logs, got {warned}"
    );
}

/// The full chain remains available at debug for local diagnosis. This is not
/// redaction: debug logs can retain raw paths and payload text.
#[tokio::test]
async fn list_files_tool_failure_keeps_the_full_chain_only_at_debug() {
    let temp = TempDir::new().expect("temporary workspace");
    let agent = build_agent(temp.path()).await;
    let tool = FileOpsTool::new(temp.path().to_path_buf(), Arc::new(GrepSearchManager::new(temp.path().to_path_buf())));
    let sentinel = "private-sentinel-debug-mode";

    let (_, debug_events) = capturing_async(
        LevelFilter::DEBUG,
        agent.run_list_files_for_tool(Some(&tool), None, &json!({"path": ".", "mode": sentinel})),
    )
    .await;

    let debugged = debug_events.join("\n");
    assert!(
        debugged.contains(sentinel) && debugged.contains("Unsupported file operation mode"),
        "the debug event must retain the suppressed chain, got {debugged}"
    );
    assert!(debugged.contains("error_category=\"listing_failed\""));
}
