//! Covers the versioned read/patch guard flow over the official ACP transport.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use std::path::PathBuf;

/// Scripted provider that replays one tool call per model turn.
struct ScriptedToolCallProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    tool_calls: Vec<(String, String)>,
    mutation_before_call: Option<(usize, PathBuf, String)>,
}

#[async_trait]
impl LLMProvider for ScriptedToolCallProvider {
    /// Return the provider identifier used to install this scripted provider.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Return the single model identifier the scripted provider advertises.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Report that the scripted provider emits streamed responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Report that the scripted provider accepts tool calls for every model.
    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    /// Accept every request without validation.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Replay the next scripted tool call and record the incoming request.
    ///
    /// An optional fixture mutation runs before the call so the test can change
    /// the file between turns.
    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.requests.lock().expect("stream requests").push(request);
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some((index, path, content)) = self.mutation_before_call.as_ref()
            && *index == response_index
        {
            std::fs::write(path, content)
                .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
        }
        let events = if let Some((tool_name, tool_arguments)) = self.tool_calls.get(response_index) {
            let response = LLMResponse {
                content: Some("Checking files.".to_string()),
                reasoning: Some("I need the current file version.".to_string()),
                tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                    format!("call-tool-{response_index}"),
                    tool_name.clone(),
                    tool_arguments.clone(),
                )]),
                finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![
                Ok(LLMStreamEvent::Token { delta: "Checking files.".to_string() }),
                Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
            ]
        } else {
            let response = LLMResponse {
                content: Some("Tool complete.".to_string()),
                finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![
                Ok(LLMStreamEvent::Token { delta: "Tool complete.".to_string() }),
                Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    /// Fail buffered generation so the test proves the streaming path is used.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Err(LLMError::InvalidRequest {
            message: "tool-enabled ACP turn used buffered generation".to_string(),
            metadata: None,
        })
    }
}

/// Answer a `fs/read_text_file` request the way an editor does: slice the
/// client-visible content with the shared 1-based line-window convention.
fn windowed_read_response(request: &acp::ReadTextFileRequest, buffer: Option<&str>) -> acp::ReadTextFileResponse {
    let content = match buffer {
        Some(buffer) => buffer.to_string(),
        None => std::fs::read_to_string(&request.path).expect("read client fixture"),
    };
    acp::ReadTextFileResponse::new(vtcode_core::line_window(&content, request.line, request.limit).to_string())
}

/// One completed scripted read/patch sequence over the official ACP transport.
struct WindowedReadFlow {
    _workspace: TempDir,
    path: PathBuf,
    calls: usize,
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    _test_lock: tokio::sync::MutexGuard<'static, ()>,
    _factory_guard: PromptProviderFactoryGuard,
}

/// Run a scripted provider turn sequence with an editor-style read client.
///
/// The client honours the requested `line`/`limit` from `client_buffer` when
/// that unsaved buffer is supplied, and from the on-disk file otherwise.
async fn run_windowed_read_flow(
    file_name: &str,
    disk_content: &str,
    client_buffer: Option<&str>,
    tool_calls: Vec<(String, String)>,
    mutation_before_call: Option<(usize, &str)>,
) -> WindowedReadFlow {
    let test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let workspace = TempDir::new().expect("wire test workspace");
    let path = workspace.path().join(file_name);
    std::fs::write(&path, disk_content).expect("write wire fixture");
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let mutation_path = path.clone();
    let mutation_before_call = mutation_before_call.map(|(index, content)| (index, mutation_path, content.to_string()));
    let factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(ScriptedToolCallProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_calls: tool_calls.clone(),
                mutation_before_call: mutation_before_call.clone(),
            })
        }),
    );
    let workspace_path = workspace.path().to_path_buf();
    let client_workspace_path = workspace_path.clone();
    let client_file_name = file_name.to_string();
    let prompt_file_name = file_name.to_string();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-read-file-version-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_buffer = client_buffer.map(str::to_string);
    let client_connection = Client
        .builder()
        .on_receive_request(
            async move |request: acp::ReadTextFileRequest, responder, _connection| {
                assert_eq!(request.path, client_workspace_path.join(&client_file_name));
                responder.respond(windowed_read_response(&request, client_buffer.as_deref()))
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
                    vec![acp::ContentBlock::Text(acp::TextContent::new(format!(
                        "Version and patch {prompt_file_name}"
                    )))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("versioned patch protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let call_count = calls.load(Ordering::SeqCst);
    WindowedReadFlow {
        _workspace: workspace,
        path,
        calls: call_count,
        requests,
        _test_lock: test_lock,
        _factory_guard: factory_guard,
    }
}

/// Drive the versioned read and no-op guard flow over the official ACP transport.
#[tokio::test]
async fn file_versions_and_no_op_guard_survive_the_official_acp_transport() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let workspace = TempDir::new().expect("wire test workspace");
    let versioned_path = workspace.path().join("versioned.txt");
    std::fs::write(&versioned_path, "before\n").expect("write versioned fixture");
    let before_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"before\n"));
    let current_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(b"current\n"));
    let read = serde_json::json!({"path": "versioned.txt", "line": 1, "limit": 1}).to_string();
    let stale = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-before\n+done\n*** End Patch\n";
    let no_op = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+current\n*** End Patch\n";
    let different = "*** Begin Patch\n*** Update File: versioned.txt\n@@\n-current\n+done\n*** End Patch\n";
    let tool_calls = vec![
        ("read_file".to_string(), read.clone()),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": stale, "expected_content_hash": before_hash.clone()}).to_string(),
        ),
        ("read_file".to_string(), read),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": no_op, "expected_content_hash": current_hash.clone()}).to_string(),
        ),
        ("apply_patch".to_string(), serde_json::json!({"patch": no_op}).to_string()),
        ("apply_patch".to_string(), serde_json::json!({"input": no_op}).to_string()),
        ("apply_patch".to_string(), serde_json::json!({"input": different}).to_string()),
    ];
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let mutation_path = versioned_path.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(ScriptedToolCallProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_calls: tool_calls.clone(),
                mutation_before_call: Some((1, mutation_path.clone(), "current\n".to_string())),
            })
        }),
    );
    let workspace_path = workspace.path().to_path_buf();
    let client_workspace_path = workspace_path.clone();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-read-file-version-test"), Arc::clone(&agent))
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
                responder.respond(windowed_read_response(&request, None))
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
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Version and patch versioned.txt",
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("versioned patch protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(calls.load(Ordering::SeqCst), 8, "all seven tool results must return to the provider");
    let requests = requests.lock().expect("stream requests");
    let payload = |index: usize| {
        let message = requests[index]
            .messages
            .iter()
            .rev()
            .find(|message| message.tool_call_id.is_some())
            .expect("tool result message");
        serde_json::from_str::<serde_json::Value>(&message.content.as_text()).expect("structured tool result")
    };
    assert_eq!(payload(1)["content_hash"], before_hash);
    assert_eq!(payload(2)["error"]["details"]["reason"], "content_hash_mismatch");
    assert_eq!(payload(3)["content_hash"], current_hash);
    assert_eq!(payload(4)["result"]["occurrence"], 1);
    assert_eq!(payload(5)["result"]["retry_prohibited"], true);
    assert_eq!(payload(6)["error"]["details"]["reason"], "repeated_identical_no_op");
    assert_eq!(payload(7)["result"]["success"], true);
    assert_eq!(std::fs::read_to_string(versioned_path).expect("read final fixture"), "done\n");
}

/// A windowed read of unchanged bytes certifies the whole-file version.
#[tokio::test]
async fn windowed_read_certifies_the_whole_file_and_patches_with_its_hash() {
    let disk = "alpha\nbeta\ngamma\n";
    let whole_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(disk.as_bytes()));
    let read = serde_json::json!({"path": "windowed.txt", "line": 2, "limit": 1}).to_string();
    let patch = "*** Begin Patch\n*** Update File: windowed.txt\n@@\n-beta\n+beta2\n*** End Patch\n";
    let tool_calls = vec![
        ("read_file".to_string(), read),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": patch, "expected_content_hash": whole_hash.clone()}).to_string(),
        ),
    ];

    let flow = run_windowed_read_flow("windowed.txt", disk, None, tool_calls, None).await;

    assert_eq!(flow.calls, 3, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    let payload = |index: usize| {
        let message = requests[index]
            .messages
            .iter()
            .rev()
            .find(|message| message.tool_call_id.is_some())
            .expect("tool result message");
        serde_json::from_str::<serde_json::Value>(&message.content.as_text()).expect("structured tool result")
    };
    assert_eq!(payload(1)["content_hash"], whole_hash);
    assert_eq!(payload(2)["result"]["success"], true);
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), "alpha\nbeta2\ngamma\n");
}

/// A dirty buffer window never certifies the on-disk file, and its stale hash
/// is rejected before any write.
#[tokio::test]
async fn windowed_read_of_a_dirty_buffer_returns_no_version_and_rejects_the_stale_hash() {
    let disk = "alpha\nbeta\ngamma\n";
    let buffer = "alpha\nBETA\ngamma\n";
    let buffer_hash = format!("sha256:{}", vtcode_commons::utils::calculate_sha256(buffer.as_bytes()));
    let read = serde_json::json!({"path": "buffer.txt", "line": 2, "limit": 1}).to_string();
    let patch = "*** Begin Patch\n*** Update File: buffer.txt\n@@\n-beta\n+beta2\n*** End Patch\n";
    let tool_calls = vec![
        ("read_file".to_string(), read),
        (
            "apply_patch".to_string(),
            serde_json::json!({"input": patch, "expected_content_hash": buffer_hash}).to_string(),
        ),
    ];

    let flow = run_windowed_read_flow("buffer.txt", disk, Some(buffer), tool_calls, None).await;

    assert_eq!(flow.calls, 3, "one result per tool call plus the final turn");
    let requests = flow.requests.lock().expect("stream requests");
    let payload = |index: usize| {
        let message = requests[index]
            .messages
            .iter()
            .rev()
            .find(|message| message.tool_call_id.is_some())
            .expect("tool result message");
        serde_json::from_str::<serde_json::Value>(&message.content.as_text()).expect("structured tool result")
    };
    assert!(payload(1)["content_hash"].is_null(), "a dirty buffer window must not certify the on-disk bytes");
    assert_eq!(payload(2)["error"]["details"]["reason"], "content_hash_mismatch");
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), disk);
}
