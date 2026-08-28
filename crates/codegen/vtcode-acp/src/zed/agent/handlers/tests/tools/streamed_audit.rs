//! Covers streamed tool calls, audit records, and the final answer.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

#[tokio::test]
async fn streamed_tool_calls_audit_success_and_failure_before_streamed_final_answer() {
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
                emit_usage: true,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "list_files",
                tool_arguments: r#"{"path":""}"#.to_string(),
                additional_tool_calls: vec![vtcode_core::llm::provider::ToolCall::function(
                    "call-unsupported".to_string(),
                    "unsupported_tool".to_string(),
                    r#"{"probe":"failure"}"#.to_string(),
                )],
            })
        }),
    );
    let launch_workspace = TempDir::new().expect("wire test launch workspace");
    let workspace = TempDir::new().expect("wire test requested workspace");
    let session_workspace = workspace.path().to_path_buf();
    let audit_path = launch_workspace.path().join("tool-audit.jsonl");
    let mut vt_config = VTCodeConfig::default();
    vt_config.acp.zed.allowed_workspace_roots.push(workspace.path().to_path_buf());
    vt_config.acp.audit =
        toml::from_str(&format!("enabled = true\npath = {:?}\nmax_size_bytes = 4096\nmax_files = 2", audit_path))
            .expect("ACP tool audit configuration");
    std::fs::write(workspace.path().join("visible.txt"), "fixture").expect("write workspace fixture");
    std::fs::write(launch_workspace.path().join("wrong-root.txt"), "fixture").expect("write launch workspace fixture");
    let launch_config = CoreAgentConfig {
        model: "wire-model".to_string(),
        api_key: "test-key".to_string(),
        provider: "wire-test".to_string(),
        api_key_env: "WIRE_TEST_API_KEY".to_string(),
        workspace: launch_workspace.path().to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "test".to_string(),
        reasoning_effort: ReasoningEffortLevel::Low,
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        max_conversation_turns: 1000,
        model_behaviour: None,
        openai_chatgpt_auth: None,
    };
    let startup = crate::zed::session::prepare_acp_agent_startup(&launch_config, &vt_config, None, true)
        .await
        .expect("prepare ACP launch with configured audit logger");
    assert!(startup.audit_logger.is_some(), "enabled ACP audit config must create the launch logger");
    let agent = Arc::new(
        crate::zed::session::create_acp_agent(startup)
            .await
            .expect("create ACP test agent"),
    );
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-stream-tool-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let audit_session_id = Arc::new(Mutex::new(None::<String>));
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(notifications_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, {
            let audit_session_id = Arc::clone(&audit_session_id);
            async move |cx: ConnectionTo<Agent>| {
                drop(
                    cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                        .block_task()
                        .await?,
                );
                let session = cx.send_request(NewSessionRequest::new(session_workspace)).block_task().await?;
                *audit_session_id.lock().expect("record ACP audit session ID") = Some(session.session_id.to_string());
                let response = cx
                    .send_request(PromptRequest::new(
                        session.session_id,
                        vec![acp::ContentBlock::Text(acp::TextContent::new("Inspect the workspace"))],
                    ))
                    .block_task()
                    .await?;
                assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
                Ok(())
            }
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("streamed tool protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let notifications = std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect::<Vec<_>>();
    let updates = notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::SessionNotification(notification) => Some(notification),
            _ => None,
        })
        .collect::<Vec<_>>();
    let usage_updates = notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::ExtNotification(notification)
                if notification.method.as_ref().trim_start_matches('_')
                    == crate::zed::agent::lody_usage::LODY_SESSION_USAGE_UPDATE_METHOD.trim_start_matches('_') =>
            {
                serde_json::from_str::<serde_json::Value>(notification.params.get()).ok()
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(usage_updates.len(), 2, "each tool-loop response must publish one usage delta");
    assert_eq!(usage_updates[0]["usage"]["inputTokens"], 100);
    assert_eq!(usage_updates[0]["usage"]["outputTokens"], 11);
    assert_eq!(usage_updates[1]["usage"]["inputTokens"], 101);
    assert_eq!(usage_updates[1]["usage"]["outputTokens"], 12);
    assert!(
        updates
            .iter()
            .all(|notification| !matches!(&notification.update, acp::SessionUpdate::Plan(_))),
        "ACP must not publish synthetic progress entries unrelated to model-managed tasks"
    );
    let visible_text = updates
        .iter()
        .filter_map(|notification| match &notification.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(visible_text.contains(&"Checking files."));
    assert!(visible_text.contains(&"Tool complete."));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one stream before and one stream after the tool");

    {
        let requests = requests.lock().expect("stream requests");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| request.stream));
        assert!(
            requests[0]
                .tools
                .as_deref()
                .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "task_tracker")),
            "ACP provider requests must expose task_tracker to the model"
        );
        assert!(
            requests[1].messages.iter().any(Message::is_tool_response),
            "the second stream must include the executed tool result"
        );
        assert_eq!(
            requests[1].messages.iter().filter(|message| message.is_tool_response()).count(),
            2,
            "the provider must receive both the successful and failed streamed tool outcomes"
        );
        let tool_response = requests[1]
            .messages
            .iter()
            .find(|message| message.is_tool_response())
            .map(|message| message.content.as_text())
            .expect("list_files tool response");
        assert!(tool_response.contains("visible.txt"), "tool must use session/new cwd: {tool_response}");
        assert!(!tool_response.contains("wrong-root.txt"), "tool must not use the ACP launch cwd: {tool_response}");
        assert!(
            requests[1]
                .messages
                .iter()
                .any(|message| message.reasoning.as_deref() == Some("I need the workspace listing.")),
            "the assistant tool-call checkpoint must retain streamed reasoning"
        );
    }

    let audit_entries = std::fs::read_to_string(&audit_path)
        .expect("read ACP tool audit JSONL")
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse ACP tool audit entry"))
        .collect::<Vec<_>>();
    assert_eq!(audit_entries.len(), 2, "each streamed ACP tool execution must be audited");
    assert!(
        audit_entries
            .iter()
            .any(|entry| entry["tool_name"] == "list_files" && entry["status"] == "success")
    );
    assert!(
        audit_entries
            .iter()
            .any(|entry| entry["tool_name"] == "unsupported_tool" && entry["status"] == "failure")
    );
    let session_id = audit_session_id
        .lock()
        .expect("read ACP audit session ID")
        .clone()
        .expect("ACP session ID recorded by the duplex client");
    assert!(audit_entries.iter().all(|entry| entry["session_id"] == session_id));
    assert!(audit_entries.iter().all(|entry| {
        entry["turn_id"].as_str().is_some_and(|turn_id| !turn_id.is_empty())
            && entry["tool_call_id"].as_str().is_some_and(|call_id| !call_id.is_empty())
            && ["arguments_hash", "result_hash"].iter().all(|field| {
                entry[*field]
                    .as_str()
                    .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
            })
    }));
    assert!(
        audit_entries
            .iter()
            .any(|entry| entry["tool_name"] == "list_files" && entry["tool_call_id"] == "call-tool")
    );
    assert!(
        audit_entries
            .iter()
            .any(|entry| { entry["tool_name"] == "unsupported_tool" && entry["tool_call_id"] == "call-unsupported" })
    );
    let turn_id = audit_entries[0]["turn_id"].as_str().expect("audit turn identifier");
    assert!(audit_entries.iter().all(|entry| entry["turn_id"] == turn_id));
    let audit_json = serde_json::to_string(&audit_entries).expect("serialize audited metadata");
    for sensitive_value in [r#""path":"""#, r#""probe":"failure""#, "visible.txt", "wrong-root.txt"] {
        assert!(!audit_json.contains(sensitive_value), "audit output leaked {sensitive_value}");
    }

    let disabled_audit_path = launch_workspace.path().join("disabled-tool-audit.jsonl");
    let mut disabled_vt_config = vt_config.clone();
    disabled_vt_config.acp.audit = toml::from_str(&format!("enabled = false\npath = {:?}", disabled_audit_path))
        .expect("disabled ACP tool audit configuration");
    let disabled_startup =
        crate::zed::session::prepare_acp_agent_startup(&launch_config, &disabled_vt_config, None, true)
            .await
            .expect("prepare ACP launch with disabled audit logging");
    assert!(
        disabled_startup.audit_logger.is_none(),
        "disabled ACP audit config must not create the launch logger"
    );
    let _disabled_agent = crate::zed::session::create_acp_agent(disabled_startup)
        .await
        .expect("create ACP test agent with audit disabled");
    assert!(!disabled_audit_path.exists(), "disabled ACP audit config must not create an audit file");
}
