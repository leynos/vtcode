//! Verifies ACP prompt streaming, partial-turn recovery, compaction, and reasoning projection.
use super::*;
use crate::audit::AcpAuditLogger;
use crate::zed::agent::handlers::prompt::{SystemToolRoundTimer, ToolRoundRequest, execute_tool_round};
use crate::zed::agent::test_support::HistorySettingsGuard;
use agent_client_protocol::schema::v1::{InitializeRequest, LoadSessionRequest, NewSessionRequest};
use diapason_config::codex::HistoryPersistence;
use diapason_config::safety::ToolAuditConfig;
use diapason_core::metrics::ToolRecoveryEvent;

mod archive_recovery;

mod compaction;

mod interrupted_tool;

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
        vec![diapason_core::llm::provider::ToolCall::function_with_namespace(
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

/// Rejects duplicate tool-call IDs before any placeholder is staged.
#[test]
fn duplicate_tool_call_ids_are_rejected_before_staging() {
    let call = |id: &str| {
        diapason_core::llm::provider::ToolCall::function(id.to_string(), "read_file".to_string(), "{}".to_string())
    };

    let error = validate_tool_call_ids(&[call("call-1"), call("call-1")])
        .expect_err("a batch with duplicate tool-call IDs must be rejected");
    assert!(
        error.to_string().contains("duplicate tool-call IDs"),
        "rejection must name the duplicate-ID cause: {error}"
    );

    validate_tool_call_ids(&[call("call-1"), call("call-2")]).expect("unique tool-call IDs must be accepted");
}

/// Rejects a duplicate-ID batch at the production boundary before staging.
///
/// The direct validator test above pins the helper. This test drives
/// `execute_tool_round`, which stages placeholders and executes tools, so it
/// proves a rejected batch reaches neither. Execution is observed through the
/// ACP tool-audit sink, which records every attempted tool call even when no
/// client is attached: the rejected duplicate round must leave the sink empty,
/// and a control round with unique IDs then appends one entry. The counter
/// assertions alone cannot prove non-invocation, because `InterruptedCall`
/// records interruptions rather than attempts.
#[tokio::test]
async fn duplicate_tool_call_ids_are_rejected_before_staging_or_execution() {
    let _history_test_guard = lock_history_test_guard().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("wire test workspace");
    let audit_path = workspace.path().join("tool-audit.jsonl");
    let audit_config: ToolAuditConfig =
        toml::from_str(&format!("enabled = true\npath = {audit_path:?}\n")).expect("audit config");
    let audit_logger = Arc::new(
        AcpAuditLogger::from_config(&audit_config)
            .await
            .expect("build ACP audit logger")
            .expect("audit logger must be enabled"),
    );
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), None, Some(audit_logger)).await);
    let audit_entries = || {
        std::fs::read_to_string(&audit_path)
            .unwrap_or_default()
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    };
    assert_eq!(audit_entries(), 0, "the audit sink must start empty");

    // Reject the duplicate batch first, while every recovery counter and the
    // audit sink are still pristine. A staged placeholder, an executed tool,
    // or a replaced result would move one of them.
    let session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create ACP session")
        .session_id;
    let session = agent
        .session_handle(&session_id)
        .expect("ACP session lookup")
        .expect("created ACP session");
    let duplicate = || {
        diapason_core::llm::provider::ToolCall::function(
            "call-duplicate".to_string(),
            "list_files".to_string(),
            r#"{"path":""}"#.to_string(),
        )
    };
    let calls = vec![duplicate(), duplicate()];
    let mut tool_loop_count = 0usize;

    let error = execute_tool_round(ToolRoundRequest {
        agent: agent.as_ref(),
        session: &session,
        session_id: &session_id,
        tool_calls: &calls,
        assistant_message: "Inspect the workspace.",
        assistant_reasoning: "",
        reasoning_details: None,
        tool_loop_count: &mut tool_loop_count,
        streamed: false,
        timer: &SystemToolRoundTimer,
    })
    .await
    .err()
    .expect("a duplicate-ID batch must be rejected before staging");

    assert!(
        error.to_string().contains("duplicate tool-call IDs"),
        "rejection must name the duplicate-ID cause: {error}"
    );
    assert_eq!(tool_loop_count, 0, "a rejected round must not advance the tool loop");
    let messages = session.data.lock().expect("wire session data").thread.messages();
    assert!(messages.is_empty(), "a rejected round must not stage history: {messages:?}");
    let metrics = agent.recovery_metrics().get_tool_recovery_metrics();
    assert_eq!(
        metrics.count(ToolRecoveryEvent::DuplicateToolCallBatch),
        1,
        "the rejected batch must be counted exactly once"
    );
    assert_eq!(metrics.count(ToolRecoveryEvent::StagedCall), 0, "no placeholder may be staged");
    assert_eq!(metrics.count(ToolRecoveryEvent::ReplacedCall), 0, "no result may be replaced");
    assert_eq!(metrics.count(ToolRecoveryEvent::InterruptedCall), 0, "no tool may be interrupted");
    assert_eq!(audit_entries(), 0, "a rejected batch must never reach the executor");

    // Control: the same harness executes a unique-ID batch, proving the audit
    // sink and the staging/replacement counters move for an admitted round.
    let control_session_id = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create control ACP session")
        .session_id;
    let control_session = agent
        .session_handle(&control_session_id)
        .expect("control ACP session lookup")
        .expect("created control ACP session");
    let control_calls = vec![diapason_core::llm::provider::ToolCall::function(
        "call-control".to_string(),
        "list_files".to_string(),
        r#"{"path":""}"#.to_string(),
    )];
    let mut control_loop_count = 0usize;
    let _control_outcome = execute_tool_round(ToolRoundRequest {
        agent: agent.as_ref(),
        session: &control_session,
        session_id: &control_session_id,
        tool_calls: &control_calls,
        assistant_message: "Inspect the workspace.",
        assistant_reasoning: "",
        reasoning_details: None,
        tool_loop_count: &mut control_loop_count,
        streamed: false,
        timer: &SystemToolRoundTimer,
    })
    .await
    .expect("a unique-ID batch must reach tool execution");
    assert_eq!(audit_entries(), 1, "the control round must record one attempted tool call");
    let control_metrics = agent.recovery_metrics().get_tool_recovery_metrics();
    assert!(
        control_metrics.count(ToolRecoveryEvent::StagedCall) >= 1,
        "the control round must stage a placeholder"
    );
    assert!(
        control_metrics.count(ToolRecoveryEvent::ReplacedCall) >= 1,
        "the control round must replace its placeholder"
    );
}

#[test]
fn reasoning_remains_visible_when_response_requests_a_tool() {
    let response = LLMResponse {
        reasoning: Some("Inspect the call graph before editing.".to_string()),
        tool_calls: Some(vec![diapason_core::llm::provider::ToolCall {
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

    let agent_connection = install_handlers(Agent.builder().name("diapason-stream-reasoning-test"), Arc::clone(&agent))
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

    let agent_connection = install_handlers(Agent.builder().name("diapason-wire-test"), Arc::clone(&agent))
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
    let acp_metadata = metadata.extension("dev.diapason.acp").and_then(serde_json::Value::as_object);
    assert_eq!(acp_metadata.and_then(|meta| meta.get("client")), Some(&serde_json::json!("wire-test")));
    assert_eq!(acp_metadata.and_then(|meta| meta.get("requestId")), Some(&serde_json::json!("prompt-1")));
    let persisted: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&archive_path).expect("read persisted wire ACP session archive"))
            .expect("parse persisted wire ACP session archive");
    assert_eq!(persisted["metadata"]["extensions"]["dev.diapason.acp"]["client"], "wire-test");
    assert_eq!(persisted["metadata"]["extensions"]["dev.diapason.acp"]["requestId"], "prompt-1");

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
            .extension("dev.diapason.acp")
            .and_then(serde_json::Value::as_object)
            .and_then(|meta| meta.get("client")),
        Some(&serde_json::json!("wire-test"))
    );
    assert_eq!(
        restored_metadata
            .extension("dev.diapason.acp")
            .and_then(serde_json::Value::as_object)
            .and_then(|meta| meta.get("requestId")),
        Some(&serde_json::json!("prompt-1"))
    );
    std::fs::remove_file(archive_path).expect("remove persisted wire ACP session archive");
}
