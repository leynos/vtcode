//! Covers ACP command argument and heredoc handling.
use super::*;

#[tokio::test]
async fn acp_local_exec_preserves_multiline_public_command_arguments() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;
    let multiline = "printf '%s' 'ACP first line\nACP second line\n'";

    for args in [
        serde_json::json!({"cmd": multiline}),
        serde_json::json!({"cmd": multiline, "justification": "exercise ACP argument routing"}),
    ] {
        let report = agent
            .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-multiline")
            .await;

        assert_eq!(
            report.status,
            acp::ToolCallStatus::Completed,
            "multiline public command failed: {}",
            report.llm_response
        );
        let payload: serde_json::Value =
            serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
        let output = payload["result"]
            .get("output")
            .or_else(|| payload["result"].get("stdout"))
            .and_then(serde_json::Value::as_str)
            .expect("successful command response must contain text output");
        assert_eq!(output, "ACP first line\nACP second line\n");
        assert!(!report.llm_response.contains("Missing required argument: command"));
    }
}

#[tokio::test]
async fn acp_local_exec_preserves_public_command_heredoc() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;
    let heredoc = "cat <<'EOF'\nACP heredoc output\nEOF";

    for args in [
        serde_json::json!({"cmd": heredoc}),
        serde_json::json!({
            "cmd": heredoc,
            "justification": "exercise ACP heredoc argument routing"
        }),
    ] {
        let report = agent
            .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-heredoc")
            .await;

        assert_eq!(
            report.status,
            acp::ToolCallStatus::Completed,
            "heredoc public command failed: {}",
            report.llm_response
        );
        let payload: serde_json::Value =
            serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
        let output = payload["result"]
            .get("output")
            .or_else(|| payload["result"].get("stdout"))
            .and_then(serde_json::Value::as_str)
            .expect("successful command response must contain text output");
        assert_eq!(output, "ACP heredoc output\n");
    }
}

#[tokio::test]
async fn acp_local_exec_rejects_missing_public_command_argument() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;

    let report = agent
        .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &serde_json::json!({}), "call-empty")
        .await;

    assert_eq!(report.status, acp::ToolCallStatus::Failed);
    assert!(report.llm_response.contains("Missing required argument: command"));
}
