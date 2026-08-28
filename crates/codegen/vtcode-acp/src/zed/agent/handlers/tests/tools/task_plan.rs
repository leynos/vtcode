//! Covers model-managed task-plan projection and replay.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

#[tokio::test]
async fn task_tracker_tool_result_emits_standard_acp_plan() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
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
                emit_usage: false,
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
