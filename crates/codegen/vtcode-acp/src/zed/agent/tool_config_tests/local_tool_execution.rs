//! Tests execution reports and ACP metadata for local tools.
use super::support::build_agent;
use crate::tooling::ToolDescriptor;
use assert_fs::TempDir;
use serde_json::json;
use std::fs;
use vtcode_core::config::constants::tools;

#[tokio::test]
async fn local_tool_execution_uses_registry_request_path() {
    let temp = TempDir::new().unwrap();
    fs::create_dir(temp.path().join("src")).unwrap();
    fs::write(temp.path().join("src").join("sample.txt"), "hello").unwrap();
    let agent = build_agent(temp.path()).await;

    let report = agent
        .execute_local_tool(
            tools::EXEC_COMMAND,
            &json!({
                "cmd": "printf sample.txt",
            }),
            "call-local-list",
        )
        .await;

    assert_eq!(report.status, crate::acp::ToolCallStatus::Completed);
    assert_eq!(report.content.len(), 3);
    let rendered_blocks = report
        .content
        .iter()
        .map(|block| {
            let crate::acp::ToolCallContent::Content(content) = block else {
                panic!("exec_command output should use ACP text content blocks");
            };
            let crate::acp::ContentBlock::Text(text) = &content.content else {
                panic!("exec_command output should be represented as ACP text");
            };
            text.text.as_str()
        })
        .collect::<Vec<_>>();
    assert!(rendered_blocks[0].contains("Command: printf sample.txt"));
    assert!(rendered_blocks[0].contains("Working directory:"));
    assert!(rendered_blocks[0].contains("Exit code: 0"));
    assert_eq!(rendered_blocks[1], "Command output\nsample.txt");
    assert!(rendered_blocks[2].contains("Execution details"));
    let payload = report.raw_output.expect("successful tool output");
    assert_eq!(payload["status"], "success");
    assert_eq!(payload["tool"], tools::EXEC_COMMAND);
    assert!(payload["result"].to_string().contains("sample.txt"));
}

#[tokio::test]
async fn local_tool_execution_reports_registry_failure() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;

    let report = agent.execute_local_tool("unknown_tool", &json!({}), "call-local-missing").await;

    assert_eq!(report.status, crate::acp::ToolCallStatus::Failed);
    assert!(report.llm_response.contains("unknown_tool"));
}

#[tokio::test]
async fn local_tool_metadata_uses_core_labels_and_kinds() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let exec_args = json!({
        "cmd": "cargo check",
    });
    let search_args = json!({
        "query": "Widget",
        "path": "src/lib.rs",
    });
    let patch_args = json!({});

    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::EXEC_COMMAND, &exec_args),
        "Run command"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::EXEC_COMMAND, Some(&exec_args)),
        crate::acp::ToolKind::Execute
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::CODE_SEARCH, &search_args),
        "Search code"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::CODE_SEARCH, Some(&search_args)),
        crate::acp::ToolKind::Search
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .render_title(ToolDescriptor::Local, tools::APPLY_PATCH, &patch_args),
        "Apply patch"
    );
    assert_eq!(
        agent
            .acp_tool_registry
            .tool_kind_for_call(tools::APPLY_PATCH, Some(&patch_args)),
        crate::acp::ToolKind::Edit
    );
}
