//! Duplicate tool-call ID rejection before staging or execution.
use super::*;
use crate::audit::AcpAuditLogger;
use crate::zed::agent::handlers::prompt::{SystemToolRoundTimer, ToolRoundRequest, execute_tool_round};
use crate::zed::agent::test_support::HistorySettingsGuard;
use agent_client_protocol::schema::v1::NewSessionRequest;
use diapason_config::codex::HistoryPersistence;
use diapason_config::safety::ToolAuditConfig;
use diapason_core::metrics::ToolRecoveryEvent;

/// Rejects duplicate tool-call IDs before any placeholder is staged.
#[test]
fn duplicate_tool_call_ids_are_rejected_before_staging() {
    let call = |id: &str| {
        diapason_core::llm::provider::ToolCall::function(id.to_string(), "read_file".to_string(), "{}".to_string())
    };

    let error = validate_tool_call_ids(&[call("call-1"), call("call-1")])
        .expect_err("a batch with duplicate tool-call IDs must be rejected");
    assert!(
        error.to_string().contains("duplicate tool-call IDs"),
        "rejection must name the duplicate-ID cause: {error}"
    );

    validate_tool_call_ids(&[call("call-1"), call("call-2")]).expect("unique tool-call IDs must be accepted");
}

/// Rejects a duplicate-ID batch at the production boundary before staging.
///
/// The direct validator test above pins the helper. This test drives
/// `execute_tool_round`, which stages placeholders and executes tools, so it
/// proves a rejected batch reaches neither. Execution is observed through the
/// ACP tool-audit sink, which records every attempted tool call even when no
/// client is attached: the rejected duplicate round must leave the sink empty,
/// and a control round with unique IDs then appends one entry. The counter
/// assertions alone cannot prove non-invocation, because `InterruptedCall`
/// records interruptions rather than attempts.
#[tokio::test]
async fn duplicate_tool_call_ids_are_rejected_before_staging_or_execution() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let audit_path = workspace.path().join("tool-audit.jsonl");
    let audit_config: ToolAuditConfig =
        toml::from_str(&format!("enabled = true\npath = {audit_path:?}\n")).expect("audit config");
    let audit_logger = Arc::new(
        AcpAuditLogger::from_config(&audit_config)
            .await
            .expect("build ACP audit logger")
            .expect("audit logger must be enabled"),
    );
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), None, Some(audit_logger)).await);
    let audit_entries = || {
        std::fs::read_to_string(&audit_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    };
    assert_eq!(audit_entries(), 0, "the audit sink must start empty");

    // Reject the duplicate batch first, while every recovery counter and the
    // audit sink are still pristine. A staged placeholder, an executed tool,
    // or a replaced result would move one of them.
    let session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;
    let session = agent
        .session_handle(&session_id)
        .expect("ACP session lookup")
        .expect("created ACP session");
    let duplicate = || {
        diapason_core::llm::provider::ToolCall::function(
            "call-duplicate".to_string(),
            "list_files".to_string(),
            r#"{"path":""}"#.to_string(),
        )
    };
    let calls = vec![duplicate(), duplicate()];
    let mut tool_loop_count = 0usize;

    let error = execute_tool_round(ToolRoundRequest {
        agent: agent.as_ref(),
        session: &session,
        session_id: &session_id,
        tool_calls: &calls,
        assistant_message: "Inspect the workspace.",
        assistant_reasoning: "",
        reasoning_details: None,
        tool_loop_count: &mut tool_loop_count,
        streamed: false,
        timer: &SystemToolRoundTimer,
    })
    .await
    .err()
    .expect("a duplicate-ID batch must be rejected before staging");

    assert!(
        error.to_string().contains("duplicate tool-call IDs"),
        "rejection must name the duplicate-ID cause: {error}"
    );
    assert_eq!(tool_loop_count, 0, "a rejected round must not advance the tool loop");
    let messages = session.data.lock().expect("wire session data").thread.messages();
    assert!(messages.is_empty(), "a rejected round must not stage history: {messages:?}");
    let metrics = agent.recovery_metrics().get_tool_recovery_metrics();
    assert_eq!(
        metrics.count(ToolRecoveryEvent::DuplicateToolCallBatch),
        1,
        "the rejected batch must be counted exactly once"
    );
    assert_eq!(metrics.count(ToolRecoveryEvent::StagedCall), 0, "no placeholder may be staged");
    assert_eq!(metrics.count(ToolRecoveryEvent::ReplacedCall), 0, "no result may be replaced");
    assert_eq!(metrics.count(ToolRecoveryEvent::InterruptedCall), 0, "no tool may be interrupted");
    assert_eq!(audit_entries(), 0, "a rejected batch must never reach the executor");

    // Control: the same harness executes a unique-ID batch, proving the audit
    // sink and the staging/replacement counters move for an admitted round.
    let control_session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create control ACP session")
        .session_id;
    let control_session = agent
        .session_handle(&control_session_id)
        .expect("control ACP session lookup")
        .expect("created control ACP session");
    let control_calls = vec![diapason_core::llm::provider::ToolCall::function(
        "call-control".to_string(),
        "list_files".to_string(),
        r#"{"path":""}"#.to_string(),
    )];
    let mut control_loop_count = 0usize;
    let _control_outcome = execute_tool_round(ToolRoundRequest {
        agent: agent.as_ref(),
        session: &control_session,
        session_id: &control_session_id,
        tool_calls: &control_calls,
        assistant_message: "Inspect the workspace.",
        assistant_reasoning: "",
        reasoning_details: None,
        tool_loop_count: &mut control_loop_count,
        streamed: false,
        timer: &SystemToolRoundTimer,
    })
    .await
    .expect("a unique-ID batch must reach tool execution");
    assert_eq!(audit_entries(), 1, "the control round must record one attempted tool call");
    let control_metrics = agent.recovery_metrics().get_tool_recovery_metrics();
    assert!(
        control_metrics.count(ToolRecoveryEvent::StagedCall) >= 1,
        "the control round must stage a placeholder"
    );
    assert!(
        control_metrics.count(ToolRecoveryEvent::ReplacedCall) >= 1,
        "the control round must replace its placeholder"
    );
}
