//! Covers model-visible file versions returned through the ACP read path.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

#[tokio::test]
async fn read_file_reports_the_full_file_version_over_acp() {
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
                tool_name: "read_file",
                tool_arguments: serde_json::json!({
                    "path": "versioned.txt",
                    "line": 1,
                    "limit": 1,
                })
                .to_string(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let file_content = "first line\nsecond line\n";
    std::fs::write(workspace.path().join("versioned.txt"), file_content).expect("write versioned fixture");
    let workspace_path = workspace.path().to_path_buf();
    let client_workspace_path = workspace_path.clone();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-read-file-version-test"),
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

    let client_connection = Client
        .builder()
        .on_receive_request(
            async move |request: acp::ReadTextFileRequest, responder, _connection| {
                assert_eq!(request.path, client_workspace_path.join("versioned.txt"));
                assert_eq!(request.line, Some(1));
                assert_eq!(request.limit, Some(1));
                responder.respond(acp::ReadTextFileResponse::new("first line\n"))
            },
            on_receive_request!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Read versioned.txt"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("read_file protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(calls.load(Ordering::SeqCst), 2, "the read result must return to the provider");
    let requests = requests.lock().expect("stream requests");
    let continuation = requests.get(1).expect("provider continuation request");
    let tool_message = continuation
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("call-tool"))
        .expect("read_file result message");
    let payload: serde_json::Value =
        serde_json::from_str(&tool_message.content.as_text()).expect("structured read_file result");
    assert_eq!(payload["content"], "first line\n");
    assert_eq!(
        payload["content_hash"],
        format!("sha256:{}", vtcode_commons::utils::calculate_sha256(file_content.as_bytes()))
    );
}
