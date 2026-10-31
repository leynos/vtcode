//! Regression coverage for buffered tool turns surfacing reasoning and commentary.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

/// Replays one tool-bearing buffered turn followed by a final answer.
struct BufferedToolThenAnswerProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMProvider for BufferedToolThenAnswerProvider {
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
            message: "buffered commentary test must not stream".to_string(),
            metadata: None,
        })
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let response_index = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if response_index == 0 {
            LLMResponse {
                content: Some("Checking files.".to_string()),
                reasoning: Some("I need the workspace listing.".to_string()),
                tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                    "call-tool".to_string(),
                    "list_files".to_string(),
                    r#"{"path":""}"#.to_string(),
                )]),
                finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            }
        } else {
            LLMResponse {
                content: Some("Tool complete.".to_string()),
                finish_reason: vtcode_core::llm::provider::FinishReason::Stop,
                model: "wire-model".to_string(),
                ..LLMResponse::default()
            }
        })
    }
}

/// Requests a tool on every turn so the shared tool-loop limit ends the turn.
struct AlwaysToolCallProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMProvider for AlwaysToolCallProvider {
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
            message: "buffered commentary test must not stream".to_string(),
            metadata: None,
        })
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let iteration = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(LLMResponse {
            content: Some(format!("Iteration {iteration}.")),
            reasoning: Some(format!("Reasoning {iteration}.")),
            tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall::function(
                format!("call-{iteration}"),
                "list_files".to_string(),
                r#"{"path":""}"#.to_string(),
            )]),
            finish_reason: vtcode_core::llm::provider::FinishReason::ToolCalls,
            model: "wire-model".to_string(),
            ..LLMResponse::default()
        })
    }
}

fn thought_text(update: &acp::SessionUpdate) -> Option<&str> {
    let acp::SessionUpdate::AgentThoughtChunk(chunk) = update else {
        return None;
    };
    match &chunk.content {
        acp::ContentBlock::Text(text) => Some(&text.text),
        _ => None,
    }
}

fn message_text(update: &acp::SessionUpdate) -> Option<&str> {
    let acp::SessionUpdate::AgentMessageChunk(chunk) = update else {
        return None;
    };
    match &chunk.content {
        acp::ContentBlock::Text(text) => Some(&text.text),
        _ => None,
    }
}

#[tokio::test]
async fn buffered_tool_turns_emit_reasoning_and_commentary_before_tool_lifecycle() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(BufferedToolThenAnswerProvider { calls: Arc::clone(&factory_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-buffered-commentary-test"),
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
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
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
        .expect("buffered commentary protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let position_of = |predicate: &dyn Fn(&acp::SessionUpdate) -> bool| {
        updates.iter().position(|notification| predicate(&notification.update))
    };

    let thought = position_of(&|update| thought_text(update) == Some("I need the workspace listing."))
        .expect("tool-iteration reasoning must reach ACP");
    let commentary = position_of(&|update| message_text(update) == Some("Checking files."))
        .expect("tool-iteration commentary must reach ACP");
    let tool_call = position_of(
        &|update| matches!(update, acp::SessionUpdate::ToolCall(call) if call.status == acp::ToolCallStatus::Pending),
    )
    .expect("the pending tool call must reach ACP");
    let tool_update = updates
        .iter()
        .rposition(|notification| matches!(&notification.update, acp::SessionUpdate::ToolCallUpdate(_)))
        .expect("the final tool update must reach ACP");
    let final_message =
        position_of(&|update| message_text(update) == Some("Tool complete.")).expect("final commentary must reach ACP");

    assert!(thought < commentary, "reasoning must precede commentary");
    assert!(commentary < tool_call, "commentary must precede the tool lifecycle");
    assert!(tool_call < tool_update, "the tool result update must follow the pending call");
    assert!(tool_update < final_message, "final commentary must follow the tool result");

    let thought_count = updates
        .iter()
        .filter(|notification| thought_text(&notification.update) == Some("I need the workspace listing."))
        .count();
    let commentary_count = updates
        .iter()
        .filter(|notification| message_text(&notification.update) == Some("Checking files."))
        .count();
    assert_eq!(thought_count, 1, "tool-iteration reasoning must be sent exactly once");
    assert_eq!(commentary_count, 1, "tool-iteration commentary must be sent exactly once");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one tool iteration and one final turn");
}

#[tokio::test]
async fn buffered_tool_limit_relays_the_refused_response_before_the_limit_message() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(AlwaysToolCallProvider { calls: Arc::clone(&factory_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let mut agent_instance = build_wire_test_agent(workspace.path()).await;
    agent_instance.tool_loop_limit = 1;
    let agent = Arc::new(agent_instance);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-buffered-tool-limit-test"),
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
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
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
        .expect("buffered tool-limit protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let position_of = |predicate: &dyn Fn(&acp::SessionUpdate) -> bool| {
        updates.iter().position(|notification| predicate(&notification.update))
    };

    let refused_reasoning = position_of(&|update| thought_text(update) == Some("Reasoning 2."))
        .expect("the refused tool response's reasoning must reach ACP");
    let refused_commentary = position_of(&|update| message_text(update) == Some("Iteration 2."))
        .expect("the refused tool response's commentary must reach ACP");
    let limit_message =
        position_of(&|update| message_text(update).is_some_and(|text| text.contains("Reached maximum tool loops (1)")))
            .expect("the tool-loop limit message must reach ACP");

    assert!(refused_reasoning < refused_commentary, "reasoning must precede commentary");
    assert!(refused_commentary < limit_message, "the refused response must be relayed before the limit message");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one executed tool round and one refused response");
}
