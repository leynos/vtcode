//! Drives the `agent` tool through normal ACP prompt execution.
//!
//! Unlike the management tests, which execute the tool directly through the
//! registry, these cases let the model emit the call and assert the ownership
//! and fail-closed guarantees across the full prompt boundary.

use super::lody_support::{
    background_record_state, close_session, expected_epoch_seconds, lody_cancel_with_reason, lody_list,
    open_build_session, spawn_agent_connection, write_lody_background_fixture,
};
use super::*;
use agent_client_protocol::schema::v1::NewSessionRequest;
use std::time::Duration;

/// Arguments the mocked model emits for its single `agent` tool call.
const AGENT_TOOL_ARGUMENTS: &str = r#"{"action":"spawn_subprocess","agent_type":"background-demo"}"#;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const FAIL_CLOSED_MESSAGE: &str = "Subagent controller is not active in this session";

pub(super) fn install_agent_tool_provider()
-> (Arc<AtomicUsize>, Arc<Mutex<Vec<LLMRequest>>>, PromptProviderFactoryGuard) {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "agent",
                tool_arguments: AGENT_TOOL_ARGUMENTS.to_string(),
                additional_tool_calls: Vec::new(),
            })
        }),
    );
    (calls, requests, guard)
}

/// Awaits the terminal ACP update for the mocked model's tool call.
pub(super) async fn terminal_tool_update(
    updates: &mut mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> agent_client_protocol::schema::v1::ToolCallUpdate {
    tokio::time::timeout(POLL_TIMEOUT, async {
        loop {
            let notification = updates.recv().await.expect("ACP session notification stream");
            if let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update {
                if update.tool_call_id.to_string() == "call-tool"
                    && matches!(
                        update.fields.status,
                        Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                    )
                {
                    break update.clone();
                }
            }
        }
    })
    .await
    .expect("the prompt-driven tool call must report a terminal ACP update")
}

pub(super) fn tool_update_text(update: &agent_client_protocol::schema::v1::ToolCallUpdate) -> String {
    update
        .fields
        .content
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|content| match content {
            acp::ToolCallContent::Content(content) => match &content.content {
                acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn assert_tool_reached_the_model(provider_calls: &AtomicUsize, provider_requests: &Mutex<Vec<LLMRequest>>) {
    assert_eq!(provider_calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");
    let requests = provider_requests.lock().expect("read provider requests");
    assert!(
        requests[0]
            .tools
            .as_deref()
            .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "agent")),
        "the provider request must expose the agent tool"
    );
    assert!(
        requests[1].messages.iter().any(|message| message.is_tool_response()),
        "the second provider request must contain the decoded ACP tool result"
    );
}

pub(super) fn assert_tool_response_contains(provider_requests: &Mutex<Vec<LLMRequest>>, expected: &str) {
    let requests = provider_requests.lock().expect("read provider requests");
    let response = requests[1]
        .messages
        .iter()
        .find(|message| message.is_tool_response())
        .map(|message| message.content.as_text())
        .expect("agent tool response");
    assert!(response.contains(expected), "the model must receive the tool result: {response}");
}

pub(super) fn delegation_prompt(session_id: acp::SessionId) -> PromptRequest {
    PromptRequest::new(
        session_id,
        vec![acp::ContentBlock::Text(acp::TextContent::new(
            "Delegate a background task",
        ))],
    )
}

#[tokio::test]
async fn prompt_driven_agent_tool_spawns_an_owned_background_task() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let (provider_calls, provider_requests, _factory_guard) = install_agent_tool_provider();
    let workspace = TempDir::new().expect("Lody prompt delegation workspace");
    write_lody_background_fixture(workspace.path());
    let launch_workspace = TempDir::new().expect("separate launch workspace");
    let mut agent = build_wire_test_agent(launch_workspace.path()).await;
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    agent.vt_config = Some(Box::new(vt_config.clone()));
    let setup_outcome = crate::zed::agent::optional_setup::attach_acp_subagent_controller(
        crate::zed::agent::AcpSubagentControllerSetup {
            registry: &agent.local_tool_registry,
            agent_config: &agent.config,
            custom_providers: &[],
            vt_config: Some(&vt_config),
            owner_session_id: None,
            workspace_gated: false,
            defer_background_restore: false,
        },
    )
    .await;
    assert_eq!(setup_outcome, crate::zed::agent::OptionalSetupOutcome::Attached);
    let agent = Arc::new(agent);
    assert!(agent.local_tool_registry.has_subagent_controller());
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "diapason-lody-prompt-delegation-test");
    let inspected_agent = Arc::clone(&agent);
    let workspace_path = workspace.path().to_path_buf();
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
            let session = open_build_session(&cx, workspace_path.clone()).await?;
            let owned_session = inspected_agent
                .session_handle(&session.session_id)
                .expect("look up the session")
                .expect("registered session");
            let controller = inspected_agent
                .session_subagent_controller(&owned_session)
                .expect("resolve the session controller")
                .expect("session has a subagent controller");
            assert!(
                controller.background_status_entries().await.is_empty(),
                "the session controller must start with no managed task"
            );
            let response = cx
                .send_request(delegation_prompt(session.session_id.clone()))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            assert_tool_reached_the_model(&provider_calls, &provider_requests);
            let update = terminal_tool_update(&mut updates_rx).await;
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Completed),
                "the prompt-driven agent call must succeed: {}",
                tool_update_text(&update)
            );
            // The provider call runs the same executor the direct management
            // test uses; the record it creates must be owned by this session.
            // The start instant is recorded when the controller settles the
            // spawned run, which a cached status read does not force, so the
            // snapshot command is used once the record exists.
            let started = tokio::time::timeout(POLL_TIMEOUT, async {
                loop {
                    let cached = controller.background_status_entries().await.into_iter().next();
                    if let Some(entry) = cached {
                        if entry.started_at.is_some() {
                            break entry;
                        }
                        if let Ok(snapshot) = controller.background_snapshot(&entry.id).await {
                            if snapshot.entry.started_at.is_some() {
                                break snapshot.entry;
                            }
                        }
                    }
                    tokio::time::sleep(POLL_INTERVAL).await;
                }
            })
            .await;
            let entry = match started {
                Ok(entry) => entry,
                Err(_) => {
                    let elsewhere = match inspected_agent.local_tool_registry.subagent_controller() {
                        Some(launch) => launch
                            .background_status_entries()
                            .await
                            .into_iter()
                            .map(|entry| (entry.id, entry.owner_session_id, entry.started_at))
                            .collect::<Vec<_>>(),
                        None => Vec::new(),
                    };
                    panic!(
                        "the prompt-driven spawn must record a started background task on the session controller; \
                         launch registry records: {elsewhere:?}"
                    );
                }
            };
            assert_eq!(
                entry.owner_session_id.as_deref(),
                Some(session.session_id.0.as_ref()),
                "the spawned task must be owned by the prompting session"
            );
            assert!(
                background_record_state(&controller, &entry.id).await.is_some(),
                "the owning session controller must track the spawned task"
            );
            let listed = lody_list(&cx, &session.session_id, false).await?;
            let tasks = listed["tasks"].as_array().expect("Lody task list");
            assert_eq!(tasks.len(), 1, "the prompt must spawn exactly one managed task: {listed}");
            assert_eq!(tasks[0]["taskId"], entry.id, "the list must report the spawned task: {listed}");
            let status = tasks[0]["status"].as_str().expect("projected task status");
            assert!(
                matches!(status, "running" | "completed"),
                "the prompt-driven task must be live or completed, got {status}: {listed}"
            );
            assert!(tasks[0]["startedAtEpochSeconds"].is_number(), "the listed start must be numeric: {listed}");
            assert_eq!(
                tasks[0]["startedAtEpochSeconds"],
                expected_epoch_seconds(entry.started_at.unwrap_or(entry.created_at)),
                "the listed start must project the controller record: {listed}"
            );
            let sibling = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;
            let sibling_list = lody_list(&cx, &sibling.session_id, false).await?;
            assert_eq!(
                sibling_list,
                serde_json::json!({ "tasks": [] }),
                "a sibling session must not see another session's task"
            );
            let denied = lody_cancel_with_reason(&cx, &sibling.session_id, &entry.id, "denied sibling")
                .await
                .expect_err("a sibling must not cancel the owned task");
            assert_eq!(
                denied.data,
                Some(serde_json::json!({ "reason": "unknown_task" })),
                "a sibling cancel must fail closed with the bounded reason"
            );
            let cancelled = controller
                .force_cancel_background(&entry.id)
                .await
                .expect("cancel the prompt-spawned background task");
            assert!(
                !cancelled.status.is_active(),
                "the prompt-spawned child process must be stopped during test cleanup"
            );
            close_session(&cx, session.session_id).await?;
            close_session(&cx, sibling.session_id).await?;
            Ok(())
        });

    tokio::time::timeout(REQUEST_TIMEOUT, client_connection)
        .await
        .expect("Lody prompt delegation client should finish")
        .expect("Lody prompt delegation protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

#[tokio::test]
async fn prompt_driven_agent_tool_fails_closed_without_a_session_controller() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let (provider_calls, provider_requests, _factory_guard) = install_agent_tool_provider();
    let workspace = TempDir::new().expect("Lody fail-closed workspace");
    write_lody_background_fixture(workspace.path());
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    // Build with subagent configuration so the launch registry advertises
    // `agent`, then clear it so session registration skips the session
    // controller while the snapshotted tool definition remains in place.
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
    assert!(agent.local_tool_registry.has_subagent_controller());
    // Empty the launch registry's controller slot and clear the runtime config
    // so session registration skips its own controller. The `agent` tool keeps
    // its snapshotted definition, but nothing can execute a spawn.
    agent.local_tool_registry = agent.local_tool_registry.with_subagent_controller(None);
    agent.vt_config = None;
    let agent = Arc::new(agent);
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "diapason-lody-prompt-fail-closed-test");
    let inspected_agent = Arc::clone(&agent);
    let workspace_path = workspace.path().to_path_buf();
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
            let session = open_build_session(&cx, workspace_path.clone()).await?;
            let owned_session = inspected_agent
                .session_handle(&session.session_id)
                .expect("look up the session")
                .expect("registered session");
            assert!(
                inspected_agent
                    .session_subagent_controller(&owned_session)
                    .expect("resolve the session controller")
                    .is_none(),
                "the fail-closed case must register no session controller"
            );
            let response = cx
                .send_request(delegation_prompt(session.session_id.clone()))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            assert_tool_reached_the_model(&provider_calls, &provider_requests);
            assert_tool_response_contains(&provider_requests, FAIL_CLOSED_MESSAGE);
            let update = terminal_tool_update(&mut updates_rx).await;
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Failed),
                "an unavailable controller must fail the tool call"
            );
            assert!(
                tool_update_text(&update).contains(FAIL_CLOSED_MESSAGE),
                "the client must receive the fail-closed message: {}",
                tool_update_text(&update)
            );
            assert!(
                inspected_agent.local_tool_registry.subagent_controller().is_none(),
                "the fail-closed case must leave no controller to spawn through"
            );
            close_session(&cx, session.session_id).await?;
            Ok(())
        });
    tokio::time::timeout(REQUEST_TIMEOUT, client_connection)
        .await
        .expect("Lody fail-closed client should finish")
        .expect("Lody fail-closed protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
