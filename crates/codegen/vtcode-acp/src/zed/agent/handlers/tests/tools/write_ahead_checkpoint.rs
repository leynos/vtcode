//! Covers the write-ahead checkpoint that must precede ACP tool execution.
use super::*;
use crate::zed::agent::test_support::{HistorySettingsGuard, lock_history_test_guard};
use agent_client_protocol::schema::v1::InitializeRequest;
use std::path::PathBuf;
use vtcode_config::codex::HistoryPersistence;

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
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        let events = if response_index == 0 {
            std::fs::remove_file(&self.archive_path)
                .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
            std::fs::create_dir(&self.archive_path)
                .map_err(|error| LLMError::Provider { message: error.to_string(), metadata: None })?;
            let response = LLMResponse {
                content: Some("I need to edit the fixture.".to_string()),
                tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                    "write-ahead-checkpoint-tool".to_string(),
                    "apply_patch".to_string(),
                    self.patch_arguments.clone(),
                )]),
                finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![Ok(LLMStreamEvent::Completed { response: Box::new(response) })]
        } else {
            let response = LLMResponse {
                content: Some("Tool complete.".to_string()),
                finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            };
            vec![Ok(LLMStreamEvent::Completed { response: Box::new(response) })]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("write-ahead checkpoint test must use streaming")
    }
}

#[tokio::test]
async fn failed_write_ahead_checkpoint_prevents_acp_tool_execution() {
    // Review 3791762676: a tool call cannot cross the execution boundary
    // when its write-ahead checkpoint did not become durable.
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let fixture = failed_write_ahead_checkpoint_fixture().await;
    let session_id = fixture.session_id.clone();

    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection =
        install_handlers(Agent.builder().name("vtcode-write-ahead-checkpoint-test"), Arc::clone(&fixture.agent))
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
            assert!(
                format!("{error:?}").contains("Failed to persist ACP session checkpoint"),
                "checkpoint failure must be visible to the ACP caller: {error:?}"
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

struct FailedWriteAheadCheckpointFixture {
    _workspace: TempDir,
    patch_path: PathBuf,
    calls: Arc<AtomicUsize>,
    _factory_guard: PromptProviderFactoryGuard,
    agent: Arc<ZedAgent>,
    session_id: acp::SessionId,
}

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
    let listing = vtcode_core::utils::session_archive::SessionListing {
        path: archive_path,
        snapshot: vtcode_core::utils::session_archive::SessionSnapshot {
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
        Some(vtcode_core::utils::session_archive::SessionArchive::resume_from_listing(&listing, metadata));
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
        patch_path,
        calls,
        _factory_guard: factory_guard,
        agent,
        session_id,
    }
}
