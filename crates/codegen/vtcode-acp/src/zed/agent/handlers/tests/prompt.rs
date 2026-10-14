use super::*;

#[test]
fn assistant_tool_call_continuation_preserves_signed_reasoning_details() {
    let signed_reasoning = serde_json::json!({
        "type": "reasoning.encrypted",
        "data": "opaque-reasoning-payload",
        "signature": "provider-signature",
    });
    let array_reasoning = serde_json::json!(["reasoning.encrypted", {"data": "opaque-array-payload"}]);
    let raw_reasoning = "provider-native reasoning detail";
    let message = assistant_tool_call_message(
        "continue with the requested tool".to_string(),
        vec![vtcode_core::llm::provider::ToolCall::function_with_namespace(
            "call-1".to_string(),
            None,
            "read_file".to_string(),
            "{\"path\":\"src/lib.rs\"}".to_string(),
        )],
        "private reasoning",
        Some(vec![
            signed_reasoning.to_string(),
            array_reasoning.to_string(),
            raw_reasoning.to_string(),
        ]),
    );

    assert_eq!(message.reasoning.as_deref(), Some("private reasoning"));
    let archived = serde_json::to_value(message).expect("assistant continuation should serialize");
    assert_eq!(archived["reasoning_details"][0], signed_reasoning);
    assert_eq!(archived["reasoning_details"][1], array_reasoning);
    assert_eq!(archived["reasoning_details"][2], serde_json::json!(raw_reasoning));
}
use vtcode_core::llm::provider::LLMError;

use super::*;

#[test]
fn reasoning_remains_visible_when_response_requests_a_tool() {
    let response = LLMResponse {
        reasoning: Some("Inspect the call graph before editing.".to_string()),
        tool_calls: Some(vec![vtcode_core::llm::provider::ToolCall {
            id: "call-1".to_string(),
            call_type: "function".to_string(),
            function: None,
            text: Some("read_file".to_string()),
            thought_signature: None,
        }]),
        ..LLMResponse::default()
    };

    let Some(acp::SessionUpdate::AgentThoughtChunk(chunk)) = response_reasoning_update(&response) else {
        panic!("reasoning should produce an ACP thought update");
    };
    let acp::ContentBlock::Text(text) = chunk.content else {
        panic!("reasoning update should contain text");
    };
    assert_eq!(text.text, "Inspect the call graph before editing.");
}

#[test]
fn partial_stream_message_preserves_output_and_marks_it_incomplete() {
    let failed_turn =
        IncompleteProviderTurn::from_failure("partial answer", "partial reasoning", "stream disconnected");
    let message = failed_turn.message;

    assert!(message.content.as_text().starts_with("partial answer"));
    assert!(message.content.as_text().contains("You can retry the prompt"));
    assert_eq!(message.reasoning.as_deref(), Some("partial reasoning"));
    let metadata = message.metadata.as_ref().expect("incomplete response metadata");
    assert!(metadata.is_incomplete());
    assert!(
        metadata
            .incomplete_reason()
            .is_some_and(|reason| reason.contains("stream disconnected"))
    );
}

#[test]
fn provider_failure_without_output_becomes_a_normal_retryable_turn() {
    let failed_turn = IncompleteProviderTurn::from_failure("", "", "\u{1b}[31m502 Bad Gateway\u{1b}[0m");

    assert_eq!(failed_turn.response.stop_reason, acp::StopReason::EndTurn);
    assert_eq!(failed_turn.message.content.as_text(), failed_turn.visible_update);
    assert!(failed_turn.visible_update.contains("You can retry the prompt"));
    assert!(failed_turn.visible_update.contains("502 Bad Gateway"));
    assert!(!failed_turn.visible_update.contains('\u{1b}'));
    let metadata = failed_turn.message.metadata.as_ref().expect("incomplete response metadata");
    assert!(metadata.is_incomplete());
    assert_eq!(metadata.incomplete_reason(), Some("502 Bad Gateway"));
}

#[tokio::test]
async fn provider_failure_is_normal_end_turn_and_same_session_accepts_a_second_prompt() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(FailThenSucceedProvider { calls: Arc::clone(&factory_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection =
        install_handlers(Agent.builder().name("vtcode-wire-test"), Arc::clone(&agent)).connect_with(agent_channel, {
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
            let _initialize = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            let session = cx
                .send_request(NewSessionRequest::new(workspace.path().to_path_buf()).meta(acp::Meta::from_iter([
                    ("client".to_string(), serde_json::json!("wire-test")),
                    ("requestId".to_string(), serde_json::json!("create-1")),
                ])))
                .block_task()
                .await?;
            let first = cx
                .send_request(
                    PromptRequest::new(
                        session.session_id.clone(),
                        vec![acp::ContentBlock::Text(acp::TextContent::new("first prompt"))],
                    )
                    .meta(acp::Meta::from_iter([("requestId".to_string(), serde_json::json!("prompt-1"))])),
                )
                .block_task()
                .await?;
            assert_eq!(first.stop_reason, acp::StopReason::EndTurn);
            let second = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("second prompt"))],
                ))
                .block_task()
                .await?;
            assert_eq!(second.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("client protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(updates.iter().any(|notification| match &notification.update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(text) => {
                text.text.contains("You can retry the prompt") && !text.text.contains('\u{1b}')
            }
            _ => false,
        },
        _ => false,
    }));
    assert!(updates.iter().any(|notification| match &notification.update {
        acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
            acp::ContentBlock::Text(text) => text.text == "second turn succeeded",
            _ => false,
        },
        _ => false,
    }));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let session = agent
        .sessions
        .lock()
        .expect("ACP session map")
        .values()
        .next()
        .expect("wire ACP session")
        .clone();
    let (metadata, archive_path) = {
        let data = session.data.lock().expect("wire ACP session data");
        let metadata = data.thread.metadata().expect("wire ACP session metadata");
        let archive_path = data.archive.as_ref().expect("wire ACP session archive").path().to_path_buf();
        (metadata, archive_path)
    };
    assert_eq!(
        metadata.client_metadata.as_ref().and_then(|meta| meta.get("client")),
        Some(&serde_json::json!("wire-test"))
    );
    assert_eq!(
        metadata.client_metadata.as_ref().and_then(|meta| meta.get("requestId")),
        Some(&serde_json::json!("prompt-1"))
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive_path).expect("read persisted wire ACP session archive"))
            .expect("parse persisted wire ACP session archive");
    assert_eq!(persisted["metadata"]["client_metadata"]["client"], "wire-test");
    assert_eq!(persisted["metadata"]["client_metadata"]["requestId"], "prompt-1");
    std::fs::remove_file(archive_path).expect("remove persisted wire ACP session archive");
}

#[tokio::test]
async fn streamed_acp_prompt_compacts_and_checkpoints_session_history() {
    const OLDEST_CONTEXT_MARKER: &str = "acp-oldest-context-marker";
    const SUMMARY_MARKER: &str = "ACP compaction summary marker";

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(CompactThenAnswerProvider {
                requests: Arc::clone(&factory_requests),
                context_size: 0,
                summary: "ACP compaction summary marker".to_string(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test compaction workspace");
    let mut vt_config = VTCodeConfig::default();
    vt_config.agent.harness.auto_compaction_enabled = true;
    vt_config.agent.harness.auto_compaction_threshold_tokens = Some(16_384);
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await);
    let client_workspace = workspace.path().to_path_buf();
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-stream-compaction-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let history_agent = Arc::clone(&agent);
    let client_requests = Arc::clone(&requests);
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |_notification: acp::SessionNotification, _cx| Ok(()),
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(client_workspace)).block_task().await?;
            let session_handle = history_agent
                .session_handle(&session.session_id)
                .expect("ACP session lookup")
                .expect("new ACP session handle");
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Prompt below the compaction threshold",
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            {
                let requests = client_requests.lock().expect("compaction provider requests");
                assert_eq!(requests.len(), 1, "a below-threshold ACP prompt must not request compaction");
                assert!(requests[0].stream, "a below-threshold ACP prompt should stream its answer directly");
            }
            for index in 0..12 {
                let user_context = if index == 0 {
                    format!("old user turn {index} {}", "older context ".repeat(12_000))
                } else {
                    format!("older user turn {index}")
                };
                history_agent
                    .push_message(&session_handle, Message::user(user_context))
                    .expect("append user history");
                let assistant_context = if index == 0 {
                    format!("{OLDEST_CONTEXT_MARKER} old assistant turn {index}")
                } else {
                    format!("older assistant turn {index}")
                };
                history_agent
                    .push_message(&session_handle, Message::assistant(assistant_context))
                    .expect("append assistant history");
            }

            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Prompt after accumulated history",
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("ACP compaction prompt should finish")
        .expect("ACP compaction protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    {
        let requests = requests.lock().expect("compaction provider requests");
        assert_eq!(requests.len(), 3, "the turn must skip small-prompt compaction, then summarize and answer");
        assert!(requests[0].stream, "the below-threshold turn must stream without a summary request");
        assert!(!requests[1].stream, "local compaction must use provider generation for its summary");
        let compaction_request_text = requests[1]
            .messages
            .iter()
            .map(|message| message.content.as_text())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            compaction_request_text.contains(OLDEST_CONTEXT_MARKER),
            "the compaction request must receive the oldest pre-compaction assistant message"
        );
        assert!(requests[2].stream, "the ACP prompt response must remain streamed after compaction");
        assert!(
            requests[2]
                .messages
                .iter()
                .any(|message| message.content.as_text().contains(SUMMARY_MARKER)),
            "the streamed prompt request must use the generated compaction summary"
        );
        assert!(
            !requests[2]
                .messages
                .iter()
                .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER)),
            "the oldest raw context must be replaced before the provider answer request"
        );
    }

    let session = agent
        .sessions
        .lock()
        .expect("ACP session map")
        .values()
        .next()
        .expect("compacted ACP session")
        .clone();
    let (history, archive_path, session_id) = {
        let data = session.data.lock().expect("compacted ACP session data");
        let archive_path = data
            .archive
            .as_ref()
            .expect("compaction checkpoint archive")
            .path()
            .to_path_buf();
        (data.thread.messages(), archive_path, data.session_id.clone())
    };
    assert!(history.iter().any(|message| message.content.as_text().contains(SUMMARY_MARKER)));
    assert!(
        !history
            .iter()
            .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER))
    );

    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive_path).expect("read ACP archive after automatic compaction"))
            .expect("parse ACP archive after automatic compaction");
    let persisted_text = persisted.to_string();
    assert!(persisted_text.contains(SUMMARY_MARKER), "the session checkpoint must persist the summary");
    assert!(
        !persisted_text.contains(OLDEST_CONTEXT_MARKER),
        "the session checkpoint must persist compacted history instead of the raw oldest message"
    );

    let restored_agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
    let _load_response = restored_agent
        .load_session(LoadSessionRequest::new(session_id.clone(), workspace.path()))
        .await
        .expect("load compacted ACP session into a fresh runtime");
    let restored_session = restored_agent
        .session_handle(&session_id)
        .expect("look up restored ACP session")
        .expect("loaded ACP session handle");
    let restored_history = restored_session
        .data
        .lock()
        .expect("restored ACP session data")
        .thread
        .messages();
    assert!(
        restored_history
            .iter()
            .any(|message| message.content.as_text().contains(SUMMARY_MARKER)),
        "restored session history must retain the compaction summary"
    );
    assert!(
        !restored_history
            .iter()
            .any(|message| message.content.as_text().contains(OLDEST_CONTEXT_MARKER)),
        "restored session history must not bring back compacted source context"
    );
    std::fs::remove_file(archive_path).expect("remove persisted ACP compaction archive");
}

#[tokio::test]
async fn prompt_over_budget_fails_closed_when_compaction_makes_no_change() {
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(false);
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    let session = large_compaction_session(&agent, workspace.path()).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = CompactThenAnswerProvider {
        requests: Arc::clone(&requests),
        context_size: 8_192,
        summary: "unused summary".to_string(),
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");

    let error = agent
        .maybe_compact_session(CompactionRequest {
            session: &session,
            provider: &provider,
            runtime: &runtime,
            model: "wire-model",
            tools: None,
        })
        .await
        .expect_err("over-budget prompt must not continue when compaction is disabled");

    assert!(error.to_string().contains("automatic compaction made no change"));
    assert!(requests.lock().expect("compaction requests").is_empty());
}

#[tokio::test]
async fn prompt_over_budget_fails_closed_when_compaction_stays_over_budget() {
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    let session = large_compaction_session(&agent, workspace.path()).await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let provider = CompactThenAnswerProvider {
        requests: Arc::clone(&requests),
        context_size: 8_192,
        summary: "oversized summary ".repeat(12_000),
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");

    let error = agent
        .maybe_compact_session(CompactionRequest {
            session: &session,
            provider: &provider,
            runtime: &runtime,
            model: "wire-model",
            tools: None,
        })
        .await
        .expect_err("over-budget compacted prompt must not continue to the provider answer");

    assert!(error.to_string().contains("still exceeds its safe provider admission budget"));
    let requests = requests.lock().expect("compaction requests");
    assert_eq!(requests.len(), 1, "only the compaction request may reach the provider");
    assert!(!requests[0].stream, "the provider request must be compaction, not an answer turn");
}

#[tokio::test]
async fn cancelled_acp_compaction_aborts_the_pending_provider_request() {
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await);
    let session = large_compaction_session(&agent, workspace.path()).await;
    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(PendingCompactionProvider::new(Arc::clone(&started)));
    let runtime = agent.provider_runtime.for_provider("wire-test");
    let task = {
        let agent = Arc::clone(&agent);
        let session = session.clone();
        let provider = Arc::clone(&provider);
        tokio::spawn(async move {
            agent
                .maybe_compact_session(CompactionRequest {
                    session: &session,
                    provider: provider.as_ref(),
                    runtime: &runtime,
                    model: "wire-model",
                    tools: None,
                })
                .await
        })
    };

    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("provider compaction request should start");
    session.cancellation.cancel();
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancellation should stop the compaction task")
        .expect("compaction task should not panic")
        .expect_err("cancelled compaction must not report success");

    assert!(error.to_string().contains("ACP compaction cancelled"));
}

#[tokio::test]
async fn visible_stream_failure_checkpoints_an_incomplete_turn_over_acp() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard =
        PromptProviderFactoryGuard::install("wire-test", Arc::new(|| Box::new(PartialThenFailProvider)));
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-partial-stream-test"), Arc::clone(&agent))
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
            let session = cx
                .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                .block_task()
                .await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Begin a response"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("partial-stream protocol flow should end normally");
    agent_task.abort();
    drop(agent_task.await);

    let visible_text = std::iter::from_fn(|| updates_rx.try_recv().ok())
        .filter_map(|notification| match notification.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
                acp::ContentBlock::Text(text) => Some(text.text),
                _ => None,
            },
            _ => None,
        })
        .collect::<String>();
    assert!(visible_text.contains("partial answer"));
    assert!(visible_text.contains("You can retry the prompt"));

    let session = agent
        .sessions
        .lock()
        .expect("ACP session map")
        .values()
        .next()
        .expect("wire session")
        .clone();
    let messages = session.data.lock().expect("wire session data").thread.messages();
    let incomplete = messages.last().expect("incomplete assistant message");
    assert!(incomplete.content.as_text().contains("partial answer"));
    assert!(incomplete.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete));
}
