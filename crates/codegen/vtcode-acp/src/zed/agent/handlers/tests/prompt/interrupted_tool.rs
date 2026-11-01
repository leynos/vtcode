//! Covers tool execution that fails after the write-ahead checkpoint succeeds.
use super::*;
use crate::zed::connection::ConnectionHandle;
use agent_client_protocol::schema::v1::NewSessionRequest;
use tokio::sync::oneshot;
use vtcode_core::llm::provider::MessageRole;
use vtcode_core::utils::session_archive::list_recent_sessions;

/// Emits one buffered tool call, then answers if the turn asks again.
struct BufferedToolCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMProvider for BufferedToolCallProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        Err(LLMError::InvalidRequest {
            message: "buffered ACP tool turn must not stream".to_string(),
            metadata: None,
        })
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        if response_index == 0 {
            Ok(LLMResponse {
                content: Some("Checking files.".to_string()),
                tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                    "call-interrupted".to_string(),
                    "apply_patch".to_string(),
                    "{}".to_string(),
                )]),
                finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            })
        } else {
            Ok(LLMResponse::new("wire-model", "Tool complete."))
        }
    }
}

/// Asserts one `apply_patch` result is an unreplayed incomplete placeholder.
fn assert_single_incomplete_result(results: &[(Option<String>, String)], origin: &str) {
    assert_eq!(results.len(), 1, "{origin} must hold exactly one tool result, not a synthetic error");
    let (tool_call_id, content) = &results[0];
    assert_eq!(
        tool_call_id.as_deref(),
        Some("call-interrupted"),
        "{origin} must keep the interrupted call's result attributed to that call"
    );
    let payload: serde_json::Value = serde_json::from_str(content).expect("incomplete tool result should be JSON");
    assert_eq!(payload["status"], "incomplete", "{origin} must keep the provisional placeholder status");
    assert_eq!(payload["tool"], "apply_patch", "{origin} must name the interrupted tool");
    assert_eq!(payload["replayed"], false, "{origin} must never claim the call was replayed");
    assert_eq!(payload["state"], "uncertain", "{origin} must record the uncertain side effects");
}

#[tokio::test]
async fn interrupted_tool_execution_keeps_one_unreplayed_incomplete_result() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(BufferedToolCallProvider { calls: Arc::clone(&factory_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;

    // The agent must see an attached client so tool execution starts, but the
    // transport is closed before the round, so the first session update that
    // reports the tool call fails. That failure lands after the write-ahead
    // checkpoint and therefore exercises the interrupted-execution path.
    let (agent_channel, client_channel) = Channel::duplex();
    let (cx_tx, cx_rx) = oneshot::channel();
    let agent_task = tokio::spawn(
        install_handlers(Agent.builder().name("vtcode-interrupted-tool-test"), Arc::clone(&agent)).connect_with(
            agent_channel,
            async move |cx: ConnectionTo<Client>| {
                drop(cx_tx.send(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            },
        ),
    );
    let cx = cx_rx.await.expect("capture the agent connection handle");
    // Close the transport from both ends before the turn starts: drop the peer
    // half and stop the agent connection task. Every later session update then
    // fails, which is the interruption this test needs. Waiting for the
    // connection task to finish on its own would never return, because its
    // closure parks on a pending future.
    drop(client_channel);
    agent_task.abort();
    drop(agent_task.await);
    agent.attach_client(ConnectionHandle::new(cx));

    assert!(
        run_prompt(
            Arc::clone(&agent),
            PromptRequest::new(
                session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new("Patch the file"))],
            ),
        )
        .await
        .is_err(),
        "interrupted tool execution must return the execution error instead of a stop response"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1, "the provider must not be called again after the interruption");

    let session = agent
        .session_handle(&session_id)
        .expect("ACP session lookup")
        .expect("created ACP session");
    let messages = session.data.lock().expect("wire session data").thread.messages();
    let tool_results = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .map(|message| (message.tool_call_id.clone(), message.content.as_text().into_owned()))
        .collect::<Vec<_>>();
    assert_single_incomplete_result(&tool_results, "thread history");

    let listings = list_recent_sessions(1).await.expect("list ACP session archives");
    let listing = listings.first().expect("the interrupted turn should be checkpointed");
    let archived_results = listing
        .snapshot
        .messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .map(|message| {
            (message.tool_call_id.as_deref().map(ToString::to_string), message.content.as_text().into_owned())
        })
        .collect::<Vec<_>>();
    assert_single_incomplete_result(&archived_results, "session archive");
}
