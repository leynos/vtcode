//! Covers the write-ahead checkpoint that must precede ACP tool execution.
use super::*;
use crate::zed::agent::handlers::prompt::{
    SystemToolRoundTimer, ToolRoundOutcome, ToolRoundRequest, execute_tool_round,
};
use crate::zed::agent::test_support::{HistorySettingsGuard, lock_history_test_guard};
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
use diapason_config::codex::HistoryPersistence;
use std::path::PathBuf;
use tokio::sync::oneshot;

/// Makes the configured archive path unusable on the first provider response.
///
/// The tool call is requested normally, so the failure the test observes comes
/// from the write-ahead checkpoint rather than from provider setup.
struct InvalidateArchiveBeforeToolProvider {
    calls: Arc<AtomicUsize>,
    archive_path: PathBuf,
    patch_arguments: String,
}

#[async_trait]
impl LLMProvider for InvalidateArchiveBeforeToolProvider {
    /// Returns the provider name used by the wire test harness.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Advertises the single wire test model.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Reports that this provider streams responses.
    fn supports_streaming(&self) -> bool {
        true
    }

    /// Reports tool support for every model.
    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    /// Accepts any request unchanged.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Breaks the archive on the first response, then returns a tool call
    /// followed by a final answer.
    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if response_index == 0 {
            std::fs::remove_file(&self.archive_path)
                .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
            std::fs::create_dir(&self.archive_path)
                .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
            let response = LLMResponse {
                content: Some("I need to edit the fixture.".to_string()),
                tool_calls: Some(vec![diapason_core::llm::provider::ToolCall::function(
                    "write-ahead-checkpoint-tool".to_string(),
                    "apply_patch".to_string(),
                    self.patch_arguments.clone(),
                )]),
                finish_reason: diapason_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![Ok(LLMStreamEvent::Completed { response: Box::new(response) })]
        } else {
            let response = LLMResponse {
                content: Some("Tool complete.".to_string()),
                finish_reason: diapason_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![Ok(LLMStreamEvent::Completed { response: Box::new(response) })]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    /// Panics because this provider only streams.
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("write-ahead checkpoint test must use streaming")
    }
}

/// Fails the prompt before execution when the write-ahead checkpoint cannot be
/// persisted.
#[tokio::test]
async fn failed_write_ahead_checkpoint_prevents_acp_tool_execution() {
    // Review 3791762676: a tool call cannot cross the execution boundary
    // when its write-ahead checkpoint did not become durable.
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let fixture = failed_write_ahead_checkpoint_fixture().await;
    let session_id = fixture.session_id.clone();
    let archive_path = fixture.archive_path.clone();

    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection =
        install_handlers(Agent.builder().name("diapason-write-ahead-checkpoint-test"), Arc::clone(&fixture.agent))
            .connect_with(agent_channel, {
                let agent = Arc::clone(&fixture.agent);
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
            let error = cx
                .send_request(PromptRequest::new(
                    session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Change the fixture"))],
                ))
                .block_task()
                .await
                .expect_err("failed write-ahead checkpoint must fail the prompt before execution");
            let rendered = format!("{error:?}");
            assert!(
                rendered.contains("Failed to persist ACP session checkpoint"),
                "checkpoint failure must be visible to the ACP caller: {rendered}"
            );
            assert!(
                rendered.contains("assistant_tool_calls_write_ahead"),
                "the client-visible error must name the checkpoint boundary: {rendered}"
            );
            assert!(
                rendered.contains("archive_write_failed"),
                "the client-visible error must carry the fixed failure category: {rendered}"
            );
            assert!(
                !rendered.contains(&archive_path.display().to_string()),
                "the client-visible error must not expose the archive path: {rendered}"
            );
            assert!(
                !rendered.contains("os error"),
                "the client-visible error must not expose the raw archive error: {rendered}"
            );
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("write-ahead checkpoint protocol flow should finish");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "the provider should not be called again after checkpoint failure"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.patch_path).expect("read patch target"),
        "original\n",
        "a failed write-ahead checkpoint must prevent tool side effects"
    );
}

/// Reads the staged call and placeholder from the durable archive while the
/// tool call is still blocked at the execution boundary.
///
/// The command reads a release gate that the test opens only after both
/// records are durable, so observing them in the archive proves the write-ahead
/// checkpoint crossed the boundary before execution completed. Moving staging
/// after execution would leave the archive without them and fail the poll.
#[cfg(unix)]
#[tokio::test]
async fn staged_call_and_placeholder_are_durable_before_tool_execution_completes() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let gate_path = workspace.path().join("release-gate");
    create_release_gate(&gate_path);
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;
    // The default duck agent denies exec_command; the build agent admits it.
    let _ = agent
        .set_session_config_option(SetSessionConfigOptionRequest::new(session_id.clone(), "primary_agent", "build"))
        .await
        .expect("select the build primary agent");
    let session = agent
        .session_handle(&session_id)
        .expect("ACP session lookup")
        .expect("created ACP session");

    // Attach a live client so tool execution can proceed. The client only
    // needs to keep the connection open; it asserts nothing in this test.
    let (agent_channel, client_channel) = Channel::duplex();
    let (cx_tx, cx_rx) = oneshot::channel();
    let agent_task =
        tokio::spawn(
            install_handlers(Agent.builder().name("diapason-execution-boundary-test"), Arc::clone(&agent))
                .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
                    drop(cx_tx.send(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }),
        );
    let cx = cx_rx.await.expect("capture the agent connection handle");
    agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));

    let call_id = "call-execution-boundary";
    // `tail` blocks opening the gate until the test opens it for writing, and
    // the execution policy admits it as a read-only command.
    let command = format!("tail {}", gate_path.display());
    let calls = vec![diapason_core::llm::provider::ToolCall::function(
        call_id.to_string(),
        "exec_command".to_string(),
        serde_json::json!({ "cmd": command }).to_string(),
    )];
    let mut tool_loop_count = 0usize;
    let mut round = tokio::spawn({
        let agent = Arc::clone(&agent);
        let session = session.clone();
        let session_id = session_id.clone();
        async move {
            execute_tool_round(ToolRoundRequest {
                agent: agent.as_ref(),
                session: &session,
                session_id: &session_id,
                tool_calls: &calls,
                assistant_message: "Run the blocking command.",
                assistant_reasoning: "",
                reasoning_details: None,
                tool_loop_count: &mut tool_loop_count,
                streamed: false,
                timer: &SystemToolRoundTimer,
            })
            .await
        }
    });

    // Poll the durable archive until the assistant call and its incomplete
    // placeholder are both present. The command is still blocked on the gate,
    // so this window is strictly before execution completes.
    let current_archive_path = || {
        session
            .data
            .lock()
            .ok()
            .and_then(|data| data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    let archive_path = loop {
        if let Some(path) = current_archive_path()
            && read_archive(&path).is_some_and(|snapshot| archive_has_staged_call(&snapshot, call_id))
        {
            break path;
        }
        // A round that already finished cannot become durable later, so report
        // its outcome instead of waiting out the deadline.
        if round.is_finished() {
            let outcome = (&mut round).await.expect("tool round task should not panic");
            panic!(
                "the tool round finished before the staged call became durable: {}; archive={}",
                describe_round_result(&outcome),
                describe_archive_at(current_archive_path().as_deref())
            );
        }
        assert!(
            Instant::now() < deadline,
            "the staged call and its placeholder must be durable before execution completes; archive={}",
            describe_archive_at(current_archive_path().as_deref())
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(!round.is_finished(), "the tool command must still be blocked at the execution boundary");
    let snapshot = read_archive(&archive_path).expect("read durable archive");
    assert!(
        archive_has_staged_call(&snapshot, call_id),
        "the durable archive must hold the assistant call and its incomplete placeholder"
    );

    release_gate(&gate_path).await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), round)
        .await
        .expect("the released tool round should finish")
        .expect("tool round task should not panic")
        .expect("the released tool round should succeed");
    assert!(matches!(outcome, ToolRoundOutcome::Continue), "the released round should continue the tool loop");

    agent_task.abort();
    drop(agent_task.await);
    drop(client_channel);
}

/// Creates the FIFO that holds the tool command at the execution boundary.
///
/// A FIFO is the only gate the execution policy admits: the allow-list has no
/// shell, so a polling loop cannot be used, and `tail` blocks opening a FIFO
/// until a writer appears.
#[cfg(unix)]
fn create_release_gate(path: &std::path::Path) {
    let status = std::process::Command::new("mkfifo")
        .arg(path)
        .status()
        .expect("mkfifo should be available to create the release gate");
    assert!(status.success(), "mkfifo must create the release gate at {}", path.display());
}

/// Opens the release gate so the blocked command observes end-of-file.
///
/// Opening for writing rendezvous with the blocked reader, so the open runs on
/// a blocking thread and is bounded in case the command never starts.
#[cfg(unix)]
async fn release_gate(path: &std::path::Path) {
    let path = path.to_path_buf();
    tokio::time::timeout(
        Duration::from_secs(10),
        tokio::task::spawn_blocking(move || std::fs::write(&path, b"release")),
    )
    .await
    .expect("the release writer should rendezvous with the blocked command")
    .expect("release writer task should not panic")
    .expect("release the blocked command");
}

/// Describes a finished tool round for a failure message.
#[cfg(unix)]
fn describe_round_result<E: std::fmt::Debug>(result: &Result<ToolRoundOutcome, E>) -> String {
    match result {
        Ok(ToolRoundOutcome::Continue) => "completed with Continue".to_string(),
        Ok(ToolRoundOutcome::Cancelled) => "completed with Cancelled".to_string(),
        Ok(ToolRoundOutcome::LimitReached(message)) => format!("completed with LimitReached({message})"),
        Err(error) => format!("failed with {error:?}"),
    }
}

/// Describes the session archive for a failure message.
///
/// The contents are bounded so a large archive cannot flood the test log.
#[cfg(unix)]
fn describe_archive_at(path: Option<&std::path::Path>) -> String {
    let Some(path) = path else {
        return "no archive attached".to_string();
    };
    match std::fs::read_to_string(path) {
        Ok(contents) => format!("{} ({})", path.display(), describe_archive_contents(&contents)),
        Err(error) => format!("{} (unreadable: {error})", path.display()),
    }
}

/// Renders at most 2000 characters of an archive document for a failure message.
#[cfg(unix)]
fn describe_archive_contents(contents: &str) -> String {
    const LIMIT: usize = 2000;
    let Some((offset, _)) = contents.char_indices().nth(LIMIT) else {
        return contents.to_string();
    };
    // `offset` comes from `char_indices`, so it is a character boundary; `get`
    // keeps the truncation UTF-8 safe without indexing into the string.
    let prefix = contents.get(..offset).unwrap_or(contents);
    format!("{prefix}…")
}

/// Parses a session archive snapshot, returning `None` while it is absent or
/// not yet a complete JSON document.
#[cfg(unix)]
fn read_archive(path: &std::path::Path) -> Option<serde_json::Value> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether the snapshot holds both the assistant tool call and its incomplete
/// placeholder for `call_id`.
#[cfg(unix)]
fn archive_has_staged_call(snapshot: &serde_json::Value, call_id: &str) -> bool {
    let Some(messages) = snapshot.get("messages").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let has_assistant_call = messages.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("Assistant")
            && message
                .get("tool_calls")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|calls| {
                    calls
                        .iter()
                        .any(|call| call.get("id").and_then(serde_json::Value::as_str) == Some(call_id))
                })
    });
    let has_placeholder = messages.iter().any(|message| {
        message.get("role").and_then(serde_json::Value::as_str) == Some("Tool")
            && message.get("tool_call_id").and_then(serde_json::Value::as_str) == Some(call_id)
            && message
                .get("content")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|content| content.contains("\"status\":\"incomplete\""))
    });
    has_assistant_call && has_placeholder
}

struct FailedWriteAheadCheckpointFixture {
    _workspace: TempDir,
    archive_path: PathBuf,
    patch_path: PathBuf,
    calls: Arc<AtomicUsize>,
    _factory_guard: PromptProviderFactoryGuard,
    agent: Arc<ZedAgent>,
    session_id: acp::SessionId,
}

/// Builds a registered session whose archive path fails the write-ahead
/// checkpoint.
async fn failed_write_ahead_checkpoint_fixture() -> FailedWriteAheadCheckpointFixture {
    let workspace = TempDir::new().expect("wire test workspace");
    let archive_path = workspace.path().join("checkpoint.json");
    let patch_path = workspace.path().join("must-not-change.txt");
    std::fs::write(&patch_path, "original\n").expect("write patch target");
    let patch = format!(
        "*** Begin Patch\n*** Update File: {}\n@@\n-original\n+changed\n*** End Patch\n",
        patch_path.display()
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let patch_arguments = serde_json::json!({"patch": patch}).to_string();
    let archive_for_provider = archive_path.clone();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session_id = agent.register_session().expect("register session");
    let session = agent
        .session_handle(&session_id)
        .expect("session lookup")
        .expect("registered session");
    let metadata = session
        .data
        .lock()
        .expect("session data")
        .thread
        .metadata()
        .expect("session metadata");
    let listing = diapason_core::utils::session_archive::SessionListing {
        path: archive_path.clone(),
        snapshot: diapason_core::utils::session_archive::SessionSnapshot {
            metadata: metadata.clone(),
            started_at: chrono::Utc::now(),
            ended_at: chrono::Utc::now(),
            total_messages: 0,
            distinct_tools: Vec::new(),
            transcript: Vec::new(),
            messages: Vec::new(),
            progress: None,
            error_logs: Vec::new(),
        },
    };
    session.data.lock().expect("session data").archive =
        Some(diapason_core::utils::session_archive::SessionArchive::resume_from_listing(&listing, metadata));
    let factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(InvalidateArchiveBeforeToolProvider {
                calls: Arc::clone(&factory_calls),
                archive_path: archive_for_provider.clone(),
                patch_arguments: patch_arguments.clone(),
            })
        }),
    );

    FailedWriteAheadCheckpointFixture {
        _workspace: workspace,
        archive_path,
        patch_path,
        calls,
        _factory_guard: factory_guard,
        agent,
        session_id,
    }
}
