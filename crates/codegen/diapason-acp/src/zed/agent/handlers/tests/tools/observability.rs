//! Covers the `acp_tool_execution` span and its bounded recovery counters.
use super::*;
use crate::zed::agent::handlers::prompt::{ToolRoundRequest, execute_tool_round};
use crate::zed::agent::test_support::HistorySettingsGuard;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use tokio::sync::oneshot;
use diapason_config::codex::HistoryPersistence;
use diapason_core::metrics::ToolRecoveryEvent;

/// Runs `body` with a thread-local trace capture and a current-thread runtime.
///
/// The runtime must be current-thread so the agent task spawned by the wire
/// harness is polled on the same thread as the installed subscriber.
fn with_trace_capture<F>(capture: TraceCapture, body: F)
where
    F: Future<Output = ()>,
{
    let subscriber = Registry::default().with(capture);
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build")
            .block_on(body);
    });
}

/// Records the `acp_tool_execution` span and one replaced-call counter.
#[test]
fn successful_tool_round_emits_acp_tool_execution_span_and_replaced_counter() {
    let capture = TraceCapture::default();
    let captured = capture.clone();
    with_trace_capture(capture, async move {
        let _history_test_guard = lock_history_test_guard().await;
        let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
        let session_dir = TempDir::new().expect("temporary ACP session archive directory");
        let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

        let calls = Arc::new(AtomicUsize::new(0));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let factory_calls = Arc::clone(&calls);
        let factory_requests = Arc::clone(&requests);
        let _factory_guard = PromptProviderFactoryGuard::install(
            "wire-test",
            Arc::new(move || {
                Box::new(StreamToolThenAnswerProvider {
                    calls: Arc::clone(&factory_calls),
                    requests: Arc::clone(&factory_requests),
                    tool_name: "list_files",
                    tool_arguments: r#"{"path":""}"#.to_string(),
                    additional_tool_calls: Vec::new(),
                })
            }),
        );

        let workspace = TempDir::new().expect("wire test workspace");
        let session_workspace = workspace.path().to_path_buf();
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let (agent_channel, client_channel) = Channel::duplex();
        let agent_connection = install_handlers(Agent.builder().name("diapason-observability-test"), Arc::clone(&agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            });
        let agent_task = tokio::spawn(agent_connection);

        let client_connection = Client
            .builder()
            .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(session_workspace)).block_task().await?;
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Inspect the workspace"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            });

        tokio::time::timeout(Duration::from_secs(5), client_connection)
            .await
            .expect("client connection should finish")
            .expect("successful tool round should complete");
        agent_task.abort();
        drop(agent_task.await);

        let trace = captured.0.lock().expect("trace capture lock").clone();
        let joined = trace.join("\n");
        assert!(
            joined.contains("span=acp_tool_execution"),
            "a successful tool round must emit the acp_tool_execution span: {joined}"
        );
        assert!(
            joined.contains("outcome=\"results_replaced\""),
            "a successful tool round must record the results_replaced outcome: {joined}"
        );
        assert!(joined.contains("elapsed_ms="), "a successful tool round must record elapsed_ms: {joined}");

        let metrics = agent.recovery_metrics().get_tool_recovery_metrics();
        assert!(
            metrics.count(ToolRecoveryEvent::ReplacedCall) >= 1,
            "a successful tool round must count at least one replaced call"
        );
        assert_eq!(
            metrics.count(ToolRecoveryEvent::InterruptedCall),
            0,
            "a successful tool round must not count an interrupted call"
        );
    });
}

/// Emits the span, bounded error category, and one interruption counter.
#[test]
fn interrupted_tool_round_emits_span_and_fixed_error_category() {
    let capture = TraceCapture::default();
    let captured = capture.clone();
    with_trace_capture(capture, async move {
        let _history_test_guard = lock_history_test_guard().await;
        let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
        let session_dir = TempDir::new().expect("temporary ACP session archive directory");
        let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
        let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

        let workspace = TempDir::new().expect("wire test workspace");
        let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
        let session_id = agent
            .new_session(NewSessionRequest::new(workspace.path()))
            .await
            .expect("create ACP session")
            .session_id;
        let session = agent
            .session_handle(&session_id)
            .expect("ACP session lookup")
            .expect("created ACP session");

        // Attach a client whose transport is already closed. The first session
        // update that reports the tool call then fails deterministically, which
        // is the interruption this test needs.
        let (agent_channel, client_channel) = Channel::duplex();
        let (cx_tx, cx_rx) = oneshot::channel();
        let agent_task = tokio::spawn(
            install_handlers(Agent.builder().name("diapason-observability-test"), Arc::clone(&agent)).connect_with(
                agent_channel,
                async move |cx: ConnectionTo<Client>| {
                    drop(cx_tx.send(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                },
            ),
        );
        let cx = cx_rx.await.expect("capture the agent connection handle");
        drop(client_channel);
        agent_task.abort();
        drop(agent_task.await);
        agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));

        let calls = vec![diapason_core::llm::provider::ToolCall::function(
            "call-observability".to_string(),
            "list_files".to_string(),
            r#"{"path":""}"#.to_string(),
        )];
        let mut tool_loop_count = 0usize;
        let error = match execute_tool_round(ToolRoundRequest {
            agent: agent.as_ref(),
            session: &session,
            session_id: &session_id,
            tool_calls: &calls,
            assistant_message: "Inspecting the workspace.",
            assistant_reasoning: "",
            reasoning_details: None,
            tool_loop_count: &mut tool_loop_count,
            streamed: false,
        })
        .await
        {
            Ok(_) => panic!("a closed client transport must interrupt the tool round"),
            Err(error) => error,
        };

        let trace = captured.0.lock().expect("trace capture lock").clone();
        let joined = trace.join("\n");
        assert!(
            joined.contains("span=acp_tool_execution"),
            "an interrupted tool round must emit the acp_tool_execution span: {joined}"
        );
        assert!(
            joined.contains("outcome=\"execution_interrupted\""),
            "an interrupted tool round must record the execution_interrupted outcome: {joined}"
        );
        assert!(joined.contains("elapsed_ms="), "an interrupted tool round must record elapsed_ms: {joined}");

        let warn_line = trace
            .iter()
            .find(|line| line.contains("tool_recovery_outcome=\"execution_interrupted\""))
            .expect("an interrupted tool round must emit a bounded interruption log");
        assert!(
            warn_line.contains("error_category=\"internal_error\""),
            "the interruption log must use a fixed error category: {warn_line}"
        );
        assert!(
            warn_line.contains("retryable=false"),
            "the interruption log must record the non-retryable contract: {warn_line}"
        );
        assert!(!warn_line.contains("error="), "the interruption log must not expose a raw error field: {warn_line}");
        let raw_error = format!("{error:?}");
        assert!(
            !warn_line.contains(&raw_error),
            "the interruption log must not expose the raw error text: {warn_line}"
        );

        let metrics = agent.recovery_metrics().get_tool_recovery_metrics();
        assert_eq!(
            metrics.count(ToolRecoveryEvent::InterruptedCall),
            1,
            "one interrupted call in the batch must count exactly one interruption"
        );
        assert_eq!(
            metrics.count(ToolRecoveryEvent::ReplacedCall),
            0,
            "an interrupted tool round must not count a replaced call"
        );
    });
}
