//! Exercises model-issued `exec_command` calls through the ACP protocol harness.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

const MULTILINE_HEREDOC_COMMAND: &str = "pwd\ncat <<'EOF'\nACP heredoc output\nEOF";

struct ExecCommandWireResult {
    launch_workspace: TempDir,
    session_workspace: TempDir,
    session_id: acp::SessionId,
    notifications: Vec<acp::SessionNotification>,
    requests: Vec<LLMRequest>,
    provider_calls: usize,
}

async fn run_exec_command_wire_case(arguments: serde_json::Value) -> ExecCommandWireResult {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let arguments = arguments.to_string();
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
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

    let agent_connection = install_handlers(Agent.builder().name("vtcode-exec-command-wire-test"), Arc::clone(&agent))
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
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, {
            let session_id = Arc::clone(&session_id);
            async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
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
                        vec![acp::ContentBlock::Text(acp::TextContent::new(
                            "Run the requested command",
                        ))],
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
        .expect("exec_command protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let session_id = session_id
        .lock()
        .expect("read ACP wire session ID")
        .clone()
        .expect("ACP wire client must create a session");
    let notifications = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect();
    let requests = requests.lock().expect("read provider requests").clone();

    ExecCommandWireResult {
        launch_workspace,
        session_workspace,
        session_id,
        notifications,
        requests,
        provider_calls: calls.load(Ordering::SeqCst),
    }
}

fn tool_update_for(notifications: &[acp::SessionNotification]) -> &agent_client_protocol::schema::v1::ToolCallUpdate {
    notifications
        .iter()
        .rev()
        .find_map(|notification| match &notification.update {
            acp::SessionUpdate::ToolCallUpdate(update) if update.tool_call_id.to_string() == "call-tool" => {
                Some(update)
            }
            _ => None,
        })
        .expect("provider tool call should produce a final ACP update")
}

fn tool_update_text(update: &agent_client_protocol::schema::v1::ToolCallUpdate) -> String {
    update
        .fields
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|content| match content {
            acp::ToolCallContent::Content(content) => match &content.content {
                acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn assert_session_routing(result: &ExecCommandWireResult) {
    assert!(
        !result.notifications.is_empty(),
        "the ACP client should receive session notifications for the model tool call"
    );
    assert!(
        result
            .notifications
            .iter()
            .all(|notification| notification.session_id == result.session_id),
        "every ACP update must be routed to the client-created session"
    );
    assert!(
        result.notifications.iter().any(|notification| matches!(
            &notification.update,
            acp::SessionUpdate::ToolCallUpdate(update)
                if update.fields.status == Some(acp::ToolCallStatus::InProgress)
        )),
        "the authorized tool call should enter the ACP in-progress state"
    );
    assert_eq!(result.provider_calls, 2, "the tool result must return to the provider");
    assert!(
        result.requests[0]
            .tools
            .as_deref()
            .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "exec_command")),
        "the provider request must expose exec_command"
    );
    assert!(
        result.requests[1].messages.iter().any(|message| message.is_tool_response()),
        "the second provider request must contain the decoded ACP tool result"
    );
}

#[tokio::test]
async fn model_exec_command_multiline_heredoc_flows_through_acp_without_justification() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let result = run_exec_command_wire_case(serde_json::json!({ "cmd": MULTILINE_HEREDOC_COMMAND })).await;

    assert_session_routing(&result);
    let update = tool_update_for(&result.notifications);
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    let output = tool_update_text(update);
    assert!(output.contains("ACP heredoc output\n"));
    assert!(output.contains(&result.session_workspace.path().to_string_lossy().to_string()));
    assert!(
        !output.contains(&result.launch_workspace.path().to_string_lossy().to_string()),
        "the command must run in the client-selected session workspace, not the launch workspace"
    );
}

#[tokio::test]
async fn model_exec_command_multiline_heredoc_flows_through_acp_with_justification() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let result = run_exec_command_wire_case(serde_json::json!({
        "cmd": MULTILINE_HEREDOC_COMMAND,
        "justification": "exercise ACP command argument routing"
    }))
    .await;

    assert_session_routing(&result);
    let update = tool_update_for(&result.notifications);
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Completed));
    let output = tool_update_text(update);
    assert!(output.contains("ACP heredoc output\n"));
    assert!(
        output.contains(&result.session_workspace.path().to_string_lossy().to_string()),
        "the command must run in the client-selected session workspace"
    );
    assert!(
        !output.contains(&result.launch_workspace.path().to_string_lossy().to_string()),
        "the command must not run in the ACP launch workspace"
    );
}

#[tokio::test]
async fn model_exec_command_empty_arguments_return_missing_command_failure_over_acp() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let result = run_exec_command_wire_case(serde_json::json!({})).await;

    assert_session_routing(&result);
    let update = tool_update_for(&result.notifications);
    assert_eq!(update.fields.status, Some(acp::ToolCallStatus::Failed));
    assert!(tool_update_text(update).contains("Missing required argument: command"));
}
