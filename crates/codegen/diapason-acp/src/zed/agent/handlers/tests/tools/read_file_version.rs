//! Covers the versioned read/patch guard flow over the official ACP transport.
use super::*;
use crate::zed::agent::test_log_capture::capturing_async;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use rstest::rstest;
use std::path::PathBuf;
use tracing_subscriber::filter::LevelFilter;

/// A fixture change applied before a scripted model turn.
#[derive(Debug, Clone)]
pub(super) enum FixtureMutation {
    /// Replace the file's contents before the turn at `index`.
    Write { index: usize, content: String },
    /// Delete the file before the turn at `index`, so the next read fails.
    Remove { index: usize },
}

/// Scripted provider that replays one tool call per model turn.
pub(super) struct ScriptedToolCallProvider {
    calls: Arc<AtomicUsize>,
    requests: Arc<Mutex<Vec<LLMRequest>>>,
    tool_calls: Vec<(String, String)>,
    mutation_before_call: Option<(FixtureMutation, PathBuf)>,
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
        if let Some((mutation, path)) = self.mutation_before_call.as_ref() {
            match mutation {
                FixtureMutation::Write { index, content } if *index == response_index => {
                    std::fs::write(path, content)
                        .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
                }
                FixtureMutation::Remove { index } if *index == response_index => {
                    std::fs::remove_file(path)
                        .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
                }
                _ => {}
            }
        }
        let events = if let Some((tool_name, tool_arguments)) = self.tool_calls.get(response_index) {
            let response = LLMResponse {
                content: Some("Checking files.".to_string()),
                reasoning: Some("I need the current file version.".to_string()),
                tool_calls: Some(vec![diapason_core::llm::provider::ToolCall::function(
                    format!("call-tool-{response_index}"),
                    tool_name.clone(),
                    tool_arguments.clone(),
                )]),
                finish_reason: diapason_core::llm::provider::FinishReason::ToolCalls,
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
                finish_reason: diapason_core::llm::provider::FinishReason::Stop,
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

/// How the scripted client answers a windowed read request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClientWindowMode {
    /// Slice the requested `line`/`limit` the way a conforming editor does.
    Honours,
    /// Return the whole client-visible content, ignoring the requested window.
    Ignores,
    /// Return only the first line of the requested window, dropping the rest.
    Truncates,
}

/// Answer a `fs/read_text_file` request the way an editor does: slice the
/// client-visible content with the shared 1-based line-window convention.
///
/// `Ignores` and `Truncates` model the two ways a client can return the wrong
/// amount of text: more than the window, or less of it.
fn windowed_read_response(
    request: &acp::ReadTextFileRequest,
    buffer: Option<&str>,
    mode: ClientWindowMode,
) -> acp::ReadTextFileResponse {
    let content = match buffer {
        Some(buffer) => buffer.to_string(),
        None => std::fs::read_to_string(&request.path).expect("read client fixture"),
    };
    let window = || diapason_core::line_window(&content, request.line, request.limit).to_string();
    let response = match mode {
        ClientWindowMode::Honours => window(),
        ClientWindowMode::Ignores => content,
        ClientWindowMode::Truncates => window().split_inclusive('\n').next().unwrap_or_default().to_string(),
    };
    acp::ReadTextFileResponse::new(response)
}

/// Read the structured tool-result payload at `index` from a recorded request.
pub(super) fn tool_result_payload(requests: &[LLMRequest], index: usize) -> serde_json::Value {
    let message = requests[index]
        .messages
        .iter()
        .rev()
        .find(|message| message.tool_call_id.is_some())
        .expect("tool result message");
    serde_json::from_str::<serde_json::Value>(&message.content.as_text()).expect("structured tool result")
}

/// One completed scripted read/patch sequence over the official ACP transport.
pub(super) struct WindowedReadFlow {
    _workspace: TempDir,
    pub(super) path: PathBuf,
    pub(super) calls: usize,
    pub(super) requests: Arc<Mutex<Vec<LLMRequest>>>,
    /// Certification counters from the registry this session actually used.
    pub(super) metrics: Arc<diapason_core::metrics::MetricsCollector>,
    /// Warnings captured while the ACP connection ran.
    pub(super) events: Vec<String>,
    _test_lock: tokio::sync::MutexGuard<'static, ()>,
    _factory_guard: PromptProviderFactoryGuard,
}

impl WindowedReadFlow {
    /// Read the certification counters for this flow.
    pub(super) fn certification(&self) -> diapason_core::metrics::PatchSafetyMetrics {
        self.metrics.get_patch_safety_metrics()
    }
}

/// Resolve the collector the session's read certification writes to.
///
/// `run_read_file` prefers the session's workspace registry and falls back to
/// the agent's launch registry, so the test reads the same one.
fn certification_metrics(
    agent: &ZedAgent,
    session_id: &acp::SessionId,
) -> Arc<diapason_core::metrics::MetricsCollector> {
    match agent.session_handle(session_id) {
        Ok(Some(handle)) => match handle.workspace_runtime() {
            Ok(Some(runtime)) => runtime.local_tool_registry.metrics_collector(),
            _ => agent.local_tool_registry.metrics_collector(),
        },
        _ => agent.local_tool_registry.metrics_collector(),
    }
}

/// Run a scripted provider turn sequence with an editor-style read client.
///
/// The client honours the requested `line`/`limit` from `client_buffer` when
/// that unsaved buffer is supplied, and from the on-disk file otherwise.
pub(super) async fn run_windowed_read_flow(
    file_name: &str,
    disk_content: &str,
    client_buffer: Option<&str>,
    tool_calls: Vec<(String, String)>,
    mutation_before_call: Option<FixtureMutation>,
    window_mode: ClientWindowMode,
) -> WindowedReadFlow {
    run_windowed_read_flow_with_bytes(
        file_name,
        disk_content,
        None,
        client_buffer,
        tool_calls,
        mutation_before_call,
        window_mode,
    )
    .await
}

/// As [`run_windowed_read_flow`], but `disk_bytes` overrides the on-disk
/// fixture so a test can write content that is not valid UTF-8.
pub(super) async fn run_windowed_read_flow_with_bytes(
    file_name: &str,
    disk_content: &str,
    disk_bytes: Option<&[u8]>,
    client_buffer: Option<&str>,
    tool_calls: Vec<(String, String)>,
    mutation_before_call: Option<FixtureMutation>,
    window_mode: ClientWindowMode,
) -> WindowedReadFlow {
    let test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let workspace = TempDir::new().expect("wire test workspace");
    let path = workspace.path().join(file_name);
    match disk_bytes {
        Some(bytes) => std::fs::write(&path, bytes).expect("write binary wire fixture"),
        None => std::fs::write(&path, disk_content).expect("write wire fixture"),
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let mutation_path = path.clone();
    let mutation_before_call = mutation_before_call.map(|mutation| (mutation, mutation_path));
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
    let session_id_slot = Arc::new(Mutex::new(None::<acp::SessionId>));
    let session_id_capture = Arc::clone(&session_id_slot);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(
        Agent.builder().name("diapason-read-file-version-test"),
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

    let client_buffer = client_buffer.map(str::to_string);
    let client_connection = Client
        .builder()
        .on_receive_request(
            async move |request: acp::ReadTextFileRequest, responder, _connection| {
                assert_eq!(request.path, client_workspace_path.join(&client_file_name));
                responder.respond(windowed_read_response(&request, client_buffer.as_deref(), window_mode))
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
            *session_id_capture.lock().expect("capture session id") = Some(session.session_id.clone());
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

    // The agent task is polled by the same current-thread runtime, so a
    // thread-local default subscriber installed here captures the warnings it
    // emits while this connection is driven.
    let (client_result, events) = capturing_async(LevelFilter::WARN, async {
        tokio::time::timeout(Duration::from_secs(5), client_connection).await
    })
    .await;
    client_result
        .expect("client connection should finish")
        .expect("versioned patch protocol flow should succeed");

    // Read the counters before the agent task is dropped; the session's
    // registry lives behind the handle the connection would tear down.
    let session_id = session_id_slot
        .lock()
        .expect("read session id")
        .clone()
        .expect("the client registered a session");
    let metrics = certification_metrics(&agent, &session_id);

    agent_task.abort();
    drop(agent_task.await);

    let call_count = calls.load(Ordering::SeqCst);
    WindowedReadFlow {
        _workspace: workspace,
        path,
        calls: call_count,
        requests,
        metrics,
        events,
        _test_lock: test_lock,
        _factory_guard: factory_guard,
    }
}

/// Drive the versioned read and no-op guard flow over the official ACP transport.
#[tokio::test]
async fn file_versions_and_no_op_guard_survive_the_official_acp_transport() {
    let before_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"before\n"));
    let current_hash = format!("sha256:{}", diapason_commons::utils::calculate_sha256(b"current\n"));
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

    let flow = run_windowed_read_flow(
        "versioned.txt",
        "before\n",
        None,
        tool_calls,
        Some(FixtureMutation::Write { index: 1, content: "current\n".to_string() }),
        ClientWindowMode::Honours,
    )
    .await;

    assert_eq!(flow.calls, 8, "all seven tool results must return to the provider");
    let requests = flow.requests.lock().expect("stream requests");
    assert_eq!(tool_result_payload(&requests, 1)["content_hash"], before_hash);
    assert_eq!(tool_result_payload(&requests, 2)["error"]["details"]["reason"], "content_hash_mismatch");
    assert_eq!(tool_result_payload(&requests, 3)["content_hash"], current_hash);
    assert_eq!(tool_result_payload(&requests, 4)["result"]["occurrence"], 1);
    assert_eq!(tool_result_payload(&requests, 5)["result"]["retry_prohibited"], true);
    assert_eq!(tool_result_payload(&requests, 6)["error"]["details"]["reason"], "repeated_identical_no_op");
    assert_eq!(tool_result_payload(&requests, 7)["result"]["success"], true);
    assert_eq!(std::fs::read_to_string(&flow.path).expect("read final fixture"), "done\n");
}
