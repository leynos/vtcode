//! Covers ACP compaction admission, cancellation, and checkpoint restoration.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, LoadSessionRequest, NewSessionRequest};

#[tokio::test]
async fn streamed_acp_prompt_compacts_and_checkpoints_session_history() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
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
    let compaction_notifications = Arc::new(Mutex::new(Vec::new()));
    let captured_notifications = Arc::clone(&compaction_notifications);
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                captured_notifications
                    .lock()
                    .expect("capture compaction notifications")
                    .push(notification);
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

    let activity_updates = compaction_notifications
        .lock()
        .expect("capture compaction notifications")
        .iter()
        .filter_map(|notification| serde_json::to_value(&notification.update).ok())
        .filter(|update| update["_meta"]["lody"]["activity"].is_object())
        .collect::<Vec<_>>();
    assert_eq!(
        activity_updates.len(),
        2,
        "a threshold crossing must publish exactly one started and one terminal compaction activity"
    );
    let started = &activity_updates[0];
    let finished = &activity_updates[1];
    assert_eq!(started["sessionUpdate"], "tool_call");
    assert_eq!(started["status"], "in_progress");
    assert_eq!(started["_meta"]["lody"]["activity"]["kind"], "context_compaction");
    assert_eq!(finished["sessionUpdate"], "tool_call_update");
    assert_eq!(finished["status"], "completed");
    assert_eq!(
        started["toolCallId"], finished["toolCallId"],
        "the terminal compaction update must keep the started tool call ID"
    );
    assert_eq!(finished["_meta"]["lody"]["activity"]["compacted"], true);
    assert!(
        finished["_meta"]["lody"]["activity"]["usedTokensAfter"]
            .as_u64()
            .is_some_and(|tokens| tokens > 0),
        "a completed compaction must report the reduced prompt size"
    );

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
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
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
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
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
