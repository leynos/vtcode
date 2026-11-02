//! Confirmation-gated `rm` deletion prompts through the ACP permission flow.

use super::*;

/// Drive one `exec_command` permission request through the duplex ACP
/// connection, returning the report and how many client permission requests
/// were observed.
async fn run_exec_command_permission_flow(
    args: Value,
    decision: ClientDecision,
    skip_confirmations: bool,
) -> (Option<ToolExecutionReport>, usize) {
    let (agent_channel, client_channel) = Channel::duplex();
    let (result_tx, result_rx) = oneshot::channel();
    let request_count = Arc::new(AtomicUsize::new(0));
    let client_request_count = Arc::clone(&request_count);
    let session_id = acp::SessionId::new("deletion-permission-test-session");
    let call = acp::ToolCall::new("deletion-permission-test-call", "Run command");

    let agent = Agent
        .builder()
        .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
            let handle = ConnectionHandle::new(cx);
            let prompter = DefaultPermissionPrompter::with_skip_confirmations(
                AcpToolRegistry::new(Path::new("/tmp"), true, true, Vec::new()),
                skip_confirmations,
            );
            let tool = PermissionToolContext::new("exec_command", acp::ToolKind::Execute, "Run command");
            let result = prompter
                .request_named_tool_permission(&handle, &session_id, &call, tool, &args)
                .await;
            drop(result_tx.send(result));
            Ok(())
        });

    let client = permission_client(decision, client_request_count, client_channel);

    let (agent_result, client_result) = tokio::join!(agent, client);
    agent_result.expect("agent duplex connection should complete");
    client_result.expect("client duplex connection should complete");
    let report = result_rx
        .await
        .expect("agent should report the permission result")
        .expect("prompter should return a result");
    (report, request_count.load(Ordering::Relaxed))
}

/// A confirmed deletion is never a read-only auto-allow: it prompts unless the
/// session already skips confirmations.
#[rstest::rstest]
#[case::prompt_when_confirmations_are_required(false, 1, true)]
#[case::bypass_prompt_when_confirmations_are_skipped(true, 0, false)]
#[tokio::test]
async fn confirmed_deletion_permission_flow(
    #[case] skip_confirmations: bool,
    #[case] expected_requests: usize,
    #[case] expects_report: bool,
) {
    let (report, request_count) = run_exec_command_permission_flow(
        json!({ "cmd": "rm -f scratch/tmp.txt", "confirm": true }),
        ClientDecision::Deny,
        skip_confirmations,
    )
    .await;

    assert_eq!(
        request_count, expected_requests,
        "skip_confirmations={skip_confirmations} must produce {expected_requests} permission request(s)"
    );
    match report {
        Some(report) => {
            assert!(expects_report, "unexpected denial report when confirmations are skipped");
            assert!(report.llm_response.contains(TOOL_PERMISSION_DENIED_MESSAGE));
        }
        None => assert!(!expects_report, "denial should produce a report"),
    }
}
