use super::*;

#[tokio::test]
async fn streamed_tool_calls_audit_success_and_failure_before_streamed_final_answer() {
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
    let audit_path = launch_workspace.path().join("tool-audit.jsonl");
    let mut vt_config = VTCodeConfig::default();
    vt_config.acp.audit =
        toml::from_str(&format!("enabled = true\npath = {:?}\nmax_size_bytes = 4096\nmax_files = 2", audit_path))
            .expect("ACP tool audit configuration");
    std::fs::write(workspace.path().join("visible.txt"), "fixture").expect("write workspace fixture");
    std::fs::write(launch_workspace.path().join("wrong-root.txt"), "fixture").expect("write launch workspace fixture");
    let agent = Arc::new(build_wire_test_agent_with_options(launch_workspace.path(), Some(&vt_config), None).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

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
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
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
                let session = cx
                    .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                    .block_task()
                    .await?;
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

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
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
}

#[tokio::test]
async fn task_tracker_tool_result_emits_standard_acp_plan() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let task_arguments = serde_json::json!({
        "action": "create",
        "title": "Real model-managed work",
        "items": [
            {
                "description": "Implement ACP plan rendering",
                "status": "in_progress",
                "files": ["src/progress.rs"],
                "verify": ["cargo test"]
            },
            {
                "description": "Publish the change",
                "status": "blocked",
                "outcome": "Waiting for credentials"
            }
        ]
    })
    .to_string();
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "task_tracker",
                tool_arguments: task_arguments.clone(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-task-plan-test"), Arc::clone(&agent))
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
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Track this work"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Continue this work"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("task tracker protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(calls.load(Ordering::SeqCst), 3, "the tracker result and next prompt must reach the provider");
    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let plans = updates
        .iter()
        .filter_map(|notification| match &notification.update {
            acp::SessionUpdate::Plan(plan) => Some(plan),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(plans.len(), 2, "the tracker result and following prompt must each publish the persisted ACP plan");
    let plan = plans[0];
    assert_eq!(plan.entries.len(), 2);
    assert_eq!(plan.entries[0].content, "Implement ACP plan rendering");
    assert_eq!(plan.entries[0].status, acp::PlanEntryStatus::InProgress);
    assert_eq!(plan.entries[1].content, "Publish the change [blocked]");
    assert_eq!(plan.entries[1].status, acp::PlanEntryStatus::Pending);
    assert_eq!(
        plan.meta.as_ref().and_then(|meta| meta.get("vtcode")),
        Some(&serde_json::json!({
            "taskTracker": {
                "title": "Real model-managed work",
                "total": 2,
                "completed": 0,
                "in_progress": 1,
                "pending": 0,
                "blocked": 1,
                "progress_percent": 0,
                "notes": null
            }
        }))
    );
    assert_eq!(plans[1], plan, "the next prompt must replay the same persisted plan");
}

#[tokio::test]
async fn approved_apply_patch_executes_and_emits_preview_and_diff_over_acp() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let patch = "*** Begin Patch\n*** Add File: patched.txt\n+created over ACP\n*** End Patch\n";
    let patch_arguments = serde_json::json!({ "patch": patch }).to_string();
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "apply_patch",
                tool_arguments: patch_arguments.clone(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-apply-patch-test"), Arc::clone(&agent))
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
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Create patched.txt"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("apply_patch protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    assert_eq!(
        std::fs::read_to_string(workspace.path().join("patched.txt")).expect("read patched file"),
        "created over ACP\n"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");

    let updates = std::iter::from_fn(|| updates_rx.try_recv().ok()).collect::<Vec<_>>();
    let preview_visible = updates.iter().any(|notification| {
        let acp::SessionUpdate::ToolCall(call) = &notification.update else {
            return false;
        };
        call.content.iter().any(|content| {
            matches!(
                content,
                acp::ToolCallContent::Content(block)
                    if matches!(&block.content, acp::ContentBlock::Text(text) if text.text == patch)
            )
        })
    });
    assert!(preview_visible, "the initial ACP tool call must expose the patch text");

    let diff = updates.iter().find_map(|notification| {
        let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
            return None;
        };
        update.fields.content.as_ref()?.iter().find_map(|content| match content {
            acp::ToolCallContent::Diff(diff) => Some(diff),
            _ => None,
        })
    });
    let diff = diff.expect("the completed ACP tool update must include a standardized diff");
    assert_eq!(diff.path, workspace.path().join("patched.txt"));
    assert_eq!(diff.old_text, None);
    assert_eq!(diff.new_text, "created over ACP\n");
}

#[tokio::test]
async fn acp_local_exec_preserves_multiline_public_command_arguments() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;
    let multiline = "printf '%s' 'ACP first line\nACP second line\n'";

    for args in [
        serde_json::json!({"cmd": multiline}),
        serde_json::json!({"cmd": multiline, "justification": "exercise ACP argument routing"}),
    ] {
        let report = agent
            .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-multiline")
            .await;

        assert_eq!(
            report.status,
            acp::ToolCallStatus::Completed,
            "multiline public command failed: {}",
            report.llm_response
        );
        let payload: serde_json::Value =
            serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
        let output = payload["result"]
            .get("output")
            .or_else(|| payload["result"].get("stdout"))
            .and_then(serde_json::Value::as_str)
            .expect("successful command response must contain text output");
        assert_eq!(output, "ACP first line\nACP second line\n");
        assert!(!report.llm_response.contains("Missing required argument: command"));
    }
}

#[tokio::test]
async fn acp_local_exec_preserves_public_command_heredoc() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;
    let heredoc = "cat <<'EOF'\nACP heredoc output\nEOF";

    for args in [
        serde_json::json!({"cmd": heredoc}),
        serde_json::json!({
            "cmd": heredoc,
            "justification": "exercise ACP heredoc argument routing"
        }),
    ] {
        let report = agent
            .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &args, "call-heredoc")
            .await;

        assert_eq!(
            report.status,
            acp::ToolCallStatus::Completed,
            "heredoc public command failed: {}",
            report.llm_response
        );
        let payload: serde_json::Value =
            serde_json::from_str(&report.llm_response).expect("successful command response must be JSON");
        let output = payload["result"]
            .get("output")
            .or_else(|| payload["result"].get("stdout"))
            .and_then(serde_json::Value::as_str)
            .expect("successful command response must contain text output");
        assert_eq!(output, "ACP heredoc output\n");
    }
}

#[tokio::test]
async fn acp_local_exec_rejects_missing_public_command_argument() {
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = build_wire_test_agent(workspace.path()).await;

    let report = agent
        .execute_local_tool(vtcode_core::config::constants::tools::EXEC_COMMAND, &serde_json::json!({}), "call-empty")
        .await;

    assert_eq!(report.status, acp::ToolCallStatus::Failed);
    assert!(report.llm_response.contains("Missing required argument: command"));
}
