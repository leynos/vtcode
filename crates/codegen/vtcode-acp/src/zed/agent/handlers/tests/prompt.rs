//! Verifies ACP prompt streaming, partial-turn recovery, compaction, and reasoning projection.
use super::*;
use crate::zed::agent::updates::{AssistantChunkKind, assistant_updates};
use agent_client_protocol::schema::v1::{InitializeRequest, LoadSessionRequest, NewSessionRequest};

mod buffered_tool_commentary;
mod compaction;
mod partial_stream;

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
#[test]
fn reasoning_and_commentary_remain_visible_when_response_requests_a_tool() {
    let response = LLMResponse {
        content: Some("Checking the call graph.".to_string()),
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

    let updates = assistant_updates(
        response.content.as_deref().unwrap_or_default(),
        response.reasoning.as_deref().unwrap_or_default(),
    );
    assert_eq!(updates.len(), 2, "a tool-bearing response must expose both channels");
    assert_eq!(updates[0].0, AssistantChunkKind::Reasoning);
    let acp::SessionUpdate::AgentThoughtChunk(thought) = &updates[0].1 else {
        panic!("reasoning should produce the first ACP thought update");
    };
    let acp::ContentBlock::Text(thought_text) = &thought.content else {
        panic!("reasoning update should contain text");
    };
    assert_eq!(thought_text.text, "Inspect the call graph before editing.");
    assert_eq!(updates[1].0, AssistantChunkKind::Commentary);
    let acp::SessionUpdate::AgentMessageChunk(message) = &updates[1].1 else {
        panic!("commentary should produce an ACP message update");
    };
    let acp::ContentBlock::Text(message_text) = &message.content else {
        panic!("commentary update should contain text");
    };
    assert_eq!(message_text.text, "Checking the call graph.");
}

#[tokio::test]
async fn streamed_reasoning_is_emitted_as_an_acp_thought_chunk_over_wire() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "list_files",
                tool_arguments: r#"{"path":""}"#.to_string(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-stream-reasoning-test"), Arc::clone(&agent))
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
        .expect("stream reasoning protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    assert!(
        updates.iter().any(|notification| matches!(
            &notification.update,
            acp::SessionUpdate::AgentThoughtChunk(chunk)
                if matches!(&chunk.content, acp::ContentBlock::Text(text) if text.text == "I need the workspace listing.")
        )),
        "streamed provider reasoning must arrive as a protocol-native ACP thought chunk"
    );
}

#[test]
fn partial_stream_message_preserves_output_and_marks_it_incomplete() {
    let failed_turn = IncompleteProviderTurn::from_failure(
        "partial answer",
        "partial reasoning",
        SafeProviderProjection::execution_error(),
    );
    let message = failed_turn.message;

    assert!(message.content.as_text().starts_with("partial answer"));
    assert!(message.content.as_text().contains("You can retry the prompt"));
    assert_eq!(message.reasoning.as_deref(), Some("partial reasoning"));
    let metadata = message.metadata.as_ref().expect("incomplete response metadata");
    assert!(metadata.is_incomplete());
    assert!(
        metadata
            .incomplete_reason()
            .is_some_and(|reason| reason.contains("Execution failed"))
    );
}

#[test]
fn provider_failure_without_output_becomes_a_normal_retryable_turn() {
    let failed_turn = IncompleteProviderTurn::from_failure(
        "",
        "",
        SafeProviderProjection::from_llm_error(&LLMError::Provider {
            message: "provider-marker".to_string(),
            metadata: Some(LLMErrorMetadata::new(
                "provider-marker",
                Some(502),
                Some("provider-marker".to_string()),
                None,
                None,
                None,
                Some("provider-marker".to_string()),
            )),
        }),
    );

    assert_eq!(failed_turn.response.stop_reason, acp::StopReason::EndTurn);
    assert_eq!(failed_turn.message.content.as_text(), failed_turn.visible_update);
    assert!(failed_turn.visible_update.contains("You can retry the prompt"));
    assert!(failed_turn.visible_update.contains("Service temporarily unavailable"));
    assert!(failed_turn.visible_update.contains("HTTP status 502"));
    assert!(!failed_turn.visible_update.contains("provider-marker"));
    let metadata = failed_turn.message.metadata.as_ref().expect("incomplete response metadata");
    assert!(metadata.is_incomplete());
    assert_eq!(
        metadata.incomplete_reason(),
        Some("Provider failure category: Service temporarily unavailable (HTTP status 502).")
    );
}

#[tokio::test]
async fn provider_failure_is_normal_end_turn_and_same_session_accepts_a_second_prompt() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let factory_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(FailThenSucceedProvider { calls: Arc::clone(&factory_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let client_workspace = workspace.path().to_path_buf();
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
                .send_request(NewSessionRequest::new(client_workspace).meta(acp::Meta::from_iter([
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
    let acp_metadata = metadata.extension("dev.vtcode.acp").and_then(serde_json::Value::as_object);
    assert_eq!(acp_metadata.and_then(|meta| meta.get("client")), Some(&serde_json::json!("wire-test")));
    assert_eq!(acp_metadata.and_then(|meta| meta.get("requestId")), Some(&serde_json::json!("prompt-1")));
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive_path).expect("read persisted wire ACP session archive"))
            .expect("parse persisted wire ACP session archive");
    assert_eq!(persisted["metadata"]["extensions"]["dev.vtcode.acp"]["client"], "wire-test");
    assert_eq!(persisted["metadata"]["extensions"]["dev.vtcode.acp"]["requestId"], "prompt-1");

    let session_id = session.data.lock().expect("wire ACP session data").session_id.clone();
    let restored_agent = build_wire_test_agent(workspace.path()).await;
    let load_response = restored_agent
        .load_session(LoadSessionRequest::new(session_id.clone(), workspace.path()))
        .await
        .expect("load persisted ACP metadata into a fresh runtime");
    assert!(load_response.config_options.is_some(), "session load must return configuration options");
    let restored_session = restored_agent
        .session_handle(&session_id)
        .expect("look up restored ACP session")
        .expect("restored ACP session handle");
    let restored_metadata = restored_session
        .data
        .lock()
        .expect("restored ACP session data")
        .thread
        .metadata()
        .unwrap();
    assert_eq!(
        restored_metadata
            .extension("dev.vtcode.acp")
            .and_then(serde_json::Value::as_object)
            .and_then(|meta| meta.get("client")),
        Some(&serde_json::json!("wire-test"))
    );
    assert_eq!(
        restored_metadata
            .extension("dev.vtcode.acp")
            .and_then(serde_json::Value::as_object)
            .and_then(|meta| meta.get("requestId")),
        Some(&serde_json::json!("prompt-1"))
    );
    std::fs::remove_file(archive_path).expect("remove persisted wire ACP session archive");
}
