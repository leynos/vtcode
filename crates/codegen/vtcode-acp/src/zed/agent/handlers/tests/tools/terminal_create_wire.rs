//! Proves that a PTY request is routed to a client advertising `terminal/*` support.
use super::*;
use agent_client_protocol::on_receive_request;
use agent_client_protocol::schema::v1::{
    ClientCapabilities, CreateTerminalRequest, CreateTerminalResponse, InitializeRequest, NewSessionRequest,
};

struct TerminalCreateWireResult {
    session_workspace: TempDir,
    session_id: acp::SessionId,
    notifications: Vec<acp::SessionNotification>,
    terminal_requests: Vec<CreateTerminalRequest>,
}

async fn run_terminal_create_wire_case() -> TerminalCreateWireResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let arguments = serde_json::json!({
        "cmd": ["printf", "terminal"],
        "tty": true,
        "working_dir": "nested",
    })
    .to_string();
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "exec_command",
                tool_arguments: arguments.clone(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    let launch_workspace = TempDir::new().expect("wire test launch workspace");
    let session_workspace = TempDir::new().expect("wire test requested session workspace");
    let session_workspace_path = session_workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(launch_workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let session_id = Arc::new(Mutex::new(None));
    let terminal_requests = Arc::new(Mutex::new(Vec::new()));

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-terminal-create-wire-test"),
        Arc::clone(&agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(&agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(agent_connection);

    let recorded_terminals = Arc::clone(&terminal_requests);
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .on_receive_request(
            async move |request: CreateTerminalRequest, responder, _cx| {
                recorded_terminals.lock().expect("record terminal request").push(request);
                responder.respond(CreateTerminalResponse::new("terminal-wire-1"))
            },
            on_receive_request!(),
        )
        .connect_with(client_channel, {
            let session_id = Arc::clone(&session_id);
            async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(
                        InitializeRequest::new(acp::ProtocolVersion::V1)
                            .client_capabilities(ClientCapabilities::new().terminal(true)),
                    )
                    .block_task()
                    .await?,
                );
                let session = cx
                    .send_request(NewSessionRequest::new(session_workspace_path))
                    .block_task()
                    .await?;
                *session_id.lock().expect("record ACP wire session ID") = Some(session.session_id.clone());
                drop(
                    cx.send_request(agent_client_protocol::schema::v1::SetSessionConfigOptionRequest::new(
                        session.session_id.clone(),
                        "primary_agent",
                        "plan",
                    ))
                    .block_task()
                    .await?,
                );
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Start a terminal"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            }
        });

    tokio::time::timeout(Duration::from_secs(30), client_connection)
        .await
        .expect("client connection should finish")
        .expect("terminal/create protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let session_id = session_id
        .lock()
        .expect("read ACP wire session ID")
        .clone()
        .expect("ACP wire client must create a session");
    let notifications = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect();
    let terminal_requests = terminal_requests.lock().expect("read terminal requests").clone();

    TerminalCreateWireResult {
        session_workspace,
        session_id,
        notifications,
        terminal_requests,
    }
}

#[tokio::test]
async fn model_pty_request_creates_a_client_terminal_in_the_session_workspace() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let result = run_terminal_create_wire_case().await;

    assert_eq!(result.terminal_requests.len(), 1, "exactly one terminal/create request is expected");
    let request = &result.terminal_requests[0];
    assert_eq!(request.session_id, result.session_id, "terminal/create must target the active session");
    assert_eq!(request.command, "printf");
    assert_eq!(request.args, vec!["terminal".to_string()]);

    let cwd = request.cwd.as_deref().expect("terminal/create must carry a working directory");
    assert!(
        cwd.starts_with(result.session_workspace.path()),
        "the terminal must start inside the client-selected session workspace, got {cwd:?}"
    );
    assert_eq!(cwd.file_name().and_then(|name| name.to_str()), Some("nested"));

    let update = result
        .notifications
        .iter()
        .rev()
        .find_map(|notification| match &notification.update {
            acp::SessionUpdate::ToolCallUpdate(update) if update.tool_call_id.to_string() == "call-tool" => {
                Some(update)
            }
            _ => None,
        })
        .expect("the client terminal should produce a final ACP tool update");
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    let terminals = update
        .fields
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|content| match content {
            acp::ToolCallContent::Terminal(terminal) => Some(terminal.terminal_id.to_string()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(terminals, vec!["terminal-wire-1".to_string()]);
}
