//! Covers the explicit failure reported when tool execution cannot complete.
use super::*;
use agent_client_protocol::schema::v1::NewSessionRequest;
use std::time::Duration;
use vtcode_core::telemetry::perf::TestPerfRecorder;

/// A provider tool call whose delivery to the client fails must fail the turn
/// with `tool_execution_failed`, never a silent `EndTurn`.
#[tokio::test]
async fn undeliverable_tool_call_reports_tool_execution_failed() {
    let _history_test_guard = lock_history_test_guard().await;
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "list_files",
                tool_arguments: "{}".to_string(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );

    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create wire test session");

    // Attach a client handle and end its connection. The handle stays
    // installed, but its channel is closed, so the tool-call updates the agent
    // sends cannot be delivered. Ending the connection before the prompt keeps
    // the failure deterministic: the outgoing channel is already gone.
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-tool-failure-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                Ok(())
            }
        });
    drop(client_channel);
    tokio::time::timeout(Duration::from_secs(5), agent_connection)
        .await
        .expect("the closed client connection should finish")
        .expect("the closed client connection should shut down cleanly");

    let error = run_prompt_with_metric_sink(
        Arc::clone(&agent),
        PromptRequest::new(
            session.session_id,
            vec![acp::ContentBlock::Text(acp::TextContent::new("list the workspace"))],
        ),
        ProviderMetricSink::test(Arc::new(TestPerfRecorder::new())),
    )
    .await
    .expect_err("a tool call that cannot be delivered must fail the turn");

    let reason = error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str);
    assert_eq!(reason, Some("tool_execution_failed"));
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the provider call must happen before tool delivery fails");
}
