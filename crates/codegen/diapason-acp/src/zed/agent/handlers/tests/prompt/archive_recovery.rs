//! Covers interrupted-archive recovery through the ACP session/load and prompt
//! protocol flow.
use super::*;
use crate::zed::agent::test_support::HistorySettingsGuard;
use crate::zed::connection::ConnectionHandle;
use agent_client_protocol::schema::v1::{InitializeRequest, LoadSessionRequest};
use chrono::Utc;
use diapason_commons::fs::write_private_json_file;
use diapason_config::codex::HistoryPersistence;
use diapason_core::llm::provider::{Message, MessageRole, ToolCall};
use diapason_core::metrics::ToolRecoveryEvent;
use diapason_core::utils::session_archive::{SessionArchiveMetadata, SessionMessage, SessionSnapshot};

/// Captures provider requests and answers every turn with plain text.
struct CapturingTextProvider {
    requests: Arc<Mutex<Vec<LLMRequest>>>,
}

#[async_trait]
impl LLMProvider for CapturingTextProvider {
    /// Returns the provider name used by the wire test harness.
    fn name(&self) -> &str {
        "wire-test"
    }

    /// Advertises the single wire test model.
    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    /// Reports that this provider never streams.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Reports tool support for every model.
    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    /// Accepts any request unchanged.
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    /// Fails streaming so the test exercises the buffered turn path.
    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        Err(LLMError::InvalidRequest {
            message: "buffered ACP archive recovery turn must not stream".to_string(),
            metadata: None,
        })
    }

    /// Records the request and answers without requesting any tool.
    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.requests.lock().expect("captured provider requests").push(request);
        Ok(LLMResponse::new("wire-model", "Recovered and continued."))
    }
}

/// Builds a persisted snapshot for an archive identifier.
fn archive_snapshot(workspace: &std::path::Path, messages: Vec<SessionMessage>) -> SessionSnapshot {
    let now = Utc::now();
    SessionSnapshot {
        metadata: SessionArchiveMetadata::new(
            "workspace",
            workspace.to_string_lossy(),
            "wire-model",
            "wire-test",
            "test",
            "low",
        ),
        started_at: now,
        ended_at: now,
        total_messages: messages.len(),
        distinct_tools: Vec::new(),
        transcript: Vec::new(),
        messages,
        progress: None,
        error_logs: Vec::new(),
    }
}

/// Returns the tool-result contents captured by the provider in order.
fn captured_tool_results(requests: &[LLMRequest]) -> Vec<(Option<String>, String)> {
    requests
        .iter()
        .flat_map(|request| request.messages.iter())
        .filter(|message| message.role == MessageRole::Tool)
        .map(|message| (message.tool_call_id.clone(), message.content.as_text().into_owned()))
        .collect()
}

/// Loads, repairs, and continues an interrupted archive over the wire.
#[tokio::test]
async fn load_and_prompt_continue_from_a_repaired_interrupted_archive() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let target = workspace.path().join("must-not-change.txt");
    std::fs::write(&target, "original\n").expect("seed the file the replayed patch would modify");
    let patch =
        format!("*** Begin Patch\n*** Update File: {}\n@@\n-original\n+replayed\n*** End Patch\n", target.display());
    let identifier = "interrupted-archive";
    let archive_path = session_dir.path().join(format!("{identifier}.json"));
    write_private_json_file(
        &archive_path,
        &archive_snapshot(
            workspace.path(),
            vec![SessionMessage::from(&Message::assistant_with_tools(
                String::new(),
                vec![ToolCall::function(
                    "legacy-call".to_string(),
                    "apply_patch".to_string(),
                    serde_json::json!({ "patch": patch }).to_string(),
                )],
            ))],
        ),
    )
    .await
    .expect("persist the interrupted archive");

    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(CapturingTextProvider { requests: Arc::clone(&factory_requests) })),
    );

    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_task = tokio::spawn(
        install_handlers(Agent.builder().name("diapason-archive-recovery-test"), Arc::clone(&agent)).connect_with(
            agent_channel,
            {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            },
        ),
    );

    let workspace_path = workspace.path().to_path_buf();
    let session_id = acp::SessionId::new(identifier);
    let load_archive_path = archive_path.clone();
    let client_connection = Client.builder().connect_with(client_channel, {
        let session_id = session_id.clone();
        async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            drop(
                cx.send_request(LoadSessionRequest::new(session_id.clone(), workspace_path))
                    .block_task()
                    .await?,
            );
            // The prompt turn checkpoints as well, so assert the repaired
            // placeholder here: between `session/load` and `session/prompt`,
            // only the load-time repair checkpoint can have written it.
            let repaired: SessionSnapshot = serde_json::from_slice(
                &std::fs::read(&load_archive_path).expect("read the archive after session/load"),
            )
            .expect("parse the archive after session/load");
            assert!(
                repaired
                    .messages
                    .iter()
                    .any(|message| message.tool_call_id.as_deref().is_some_and(|id| id == "legacy-call")),
                "session/load must persist the repaired placeholder before the prompt runs"
            );
            let response = cx
                .send_request(PromptRequest::new(
                    session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Continue"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        }
    });

    tokio::time::timeout(Duration::from_secs(10), client_connection)
        .await
        .expect("the archive recovery protocol flow should finish")
        .expect("session/load and session/prompt should succeed over the wire");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(
        std::fs::read_to_string(&target).expect("read the file the replayed patch would modify"),
        "original\n",
        "the repaired call must not be replayed"
    );

    let broker_requests = requests.lock().expect("captured provider requests");
    let results = captured_tool_results(&broker_requests);
    assert_eq!(results.len(), 1, "the provider must receive the repaired result, not a replay");
    assert_eq!(results[0].0.as_deref(), Some("legacy-call"));
    let payload: serde_json::Value =
        serde_json::from_str(&results[0].1).expect("the incomplete result should be structured JSON");
    assert_eq!(payload["status"], "incomplete");
    assert_eq!(payload["replayed"], false);
    drop(broker_requests);
}

/// Rejects an ambiguous archive over the wire without rewriting it.
#[tokio::test]
async fn ambiguous_archive_load_fails_and_preserves_the_archive() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let identifier = "ambiguous-archive";
    let archive_path = session_dir.path().join(format!("{identifier}.json"));
    let snapshot = archive_snapshot(
        workspace.path(),
        vec![
            SessionMessage::from(&Message::assistant_with_tools(
                String::new(),
                vec![ToolCall::function(
                    "ambiguous-call".to_string(),
                    "apply_patch".to_string(),
                    "{}".to_string(),
                )],
            )),
            SessionMessage::from(&Message::tool_response(
                "ambiguous-call".to_string(),
                "first terminal result".to_string(),
            )),
            SessionMessage::from(&Message::tool_response(
                "ambiguous-call".to_string(),
                "conflicting terminal result".to_string(),
            )),
        ],
    );
    write_private_json_file(&archive_path, &snapshot)
        .await
        .expect("persist the ambiguous archive");
    let original_bytes = std::fs::read(&archive_path).expect("read the ambiguous archive");

    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_task = tokio::spawn(
        install_handlers(Agent.builder().name("diapason-ambiguous-archive-test"), Arc::clone(&agent)).connect_with(
            agent_channel,
            {
                let agent = Arc::clone(&agent);
                async move |cx: ConnectionTo<Client>| {
                    agent.attach_client(ConnectionHandle::new(cx));
                    std::future::pending::<agent_client_protocol::Result<()>>().await
                }
            },
        ),
    );

    let load_failed = Arc::new(AtomicUsize::new(0));
    let client_failed = Arc::clone(&load_failed);
    let workspace_path = workspace.path().to_path_buf();
    let session_id = acp::SessionId::new(identifier);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            if cx
                .send_request(LoadSessionRequest::new(session_id, workspace_path))
                .block_task()
                .await
                .is_err()
            {
                let _ = client_failed.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        });
    tokio::time::timeout(Duration::from_secs(10), client_connection)
        .await
        .expect("the ambiguous-archive protocol flow should finish")
        .expect("the client connection should stay healthy after a rejected load");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(load_failed.load(Ordering::SeqCst), 1, "session/load must reject ambiguous history");
    assert_eq!(
        agent
            .recovery_metrics()
            .get_tool_recovery_metrics()
            .count(ToolRecoveryEvent::AmbiguousHistory),
        1,
        "the rejected load must reach the ambiguity path"
    );
    assert_eq!(
        std::fs::read(&archive_path).expect("read the ambiguous archive after rejection"),
        original_bytes,
        "a rejected archive must not be rewritten"
    );
}
