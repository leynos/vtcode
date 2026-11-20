//! Drives the `agent` tool through normal ACP prompt execution.
//!
//! Unlike the management tests, which execute the tool directly through the
//! registry, these cases let the model emit the call and assert the ownership
//! and fail-closed guarantees across the full prompt boundary.

use super::lody_delegation_fixtures::{DelegationTestGuards, delegation_test_guards};
use super::lody_support::{
    background_record_state, close_session, expected_epoch_seconds, lody_cancel_with_reason, lody_list,
    open_build_session, spawn_agent_connection, write_lody_background_fixture,
};
use super::*;
use agent_client_protocol::schema::v1::NewSessionRequest;
use diapason_core::subagents::TaskOwner;
use rstest::rstest;
use std::time::Duration;

/// Arguments the mocked model emits for its single `agent` tool call.
const AGENT_TOOL_ARGUMENTS: &str = r#"{"action":"spawn_subprocess","agent_type":"background-demo"}"#;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
pub(super) const FAIL_CLOSED_MESSAGE: &str = "Subagent controller is not active in this session";

pub(super) fn install_agent_tool_provider()
-> (Arc<AtomicUsize>, Arc<Mutex<Vec<LLMRequest>>>, PromptProviderFactoryGuard) {
    install_agent_tool_provider_with_arguments(AGENT_TOOL_ARGUMENTS)
}

/// Installs the mocked agent-tool provider with caller-supplied arguments.
///
/// A test that needs the model to emit a specific target uses this instead of
/// the fixture default; the returned guards replace the fixture's provider for
/// the duration of the test.
pub(super) fn install_agent_tool_provider_with_arguments(
    tool_arguments: &str,
) -> (Arc<AtomicUsize>, Arc<Mutex<Vec<LLMRequest>>>, PromptProviderFactoryGuard) {
    let calls = Arc::new(AtomicUsize::new(0));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let factory_calls = Arc::clone(&calls);
    let factory_requests = Arc::clone(&requests);
    let tool_arguments = tool_arguments.to_string();
    let guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(StreamToolThenAnswerProvider {
                emit_usage: false,
                calls: Arc::clone(&factory_calls),
                requests: Arc::clone(&factory_requests),
                tool_name: "agent",
                tool_arguments: tool_arguments.clone(),
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
    terminal_update_in(&tool_notifications_through_terminal(updates).await)
}

/// Collects every notification up to and including the mocked model's
/// terminal ACP update.
///
/// Ordering is part of the contract a client observes: a call must be
/// announced before it settles. Retaining the notifications lets a test assert
/// that order instead of seeing only the final update.
pub(super) async fn tool_notifications_through_terminal(
    updates: &mut mpsc::UnboundedReceiver<acp::SessionNotification>,
) -> Vec<acp::SessionNotification> {
    tokio::time::timeout(POLL_TIMEOUT, async {
        let mut received = Vec::new();
        loop {
            let notification = updates.recv().await.expect("ACP session notification stream");
            let terminal = matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.to_string() == "call-tool"
                        && matches!(
                            update.fields.status,
                            Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                        )
            );
            received.push(notification);
            if terminal {
                break received;
            }
        }
    })
    .await
    .expect("the prompt-driven tool call must report a terminal ACP update")
}

/// Extracts the terminal update from a collected notification history.
pub(super) fn terminal_update_in(
    notifications: &[acp::SessionNotification],
) -> agent_client_protocol::schema::v1::ToolCallUpdate {
    notifications
        .iter()
        .rev()
        .find_map(|notification| match &notification.update {
            acp::SessionUpdate::ToolCallUpdate(update)
                if update.tool_call_id.to_string() == "call-tool"
                    && matches!(
                        update.fields.status,
                        Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                    ) =>
            {
                Some(update.clone())
            }
            _ => None,
        })
        .expect("the prompt-driven tool call must report a terminal ACP update")
}

/// Asserts the client saw the call announced before it settled.
///
/// A rejected call is never `InProgress`, so the only well-formed sequence is
/// the initial `ToolCall` followed by the terminal `ToolCallUpdate`; a client
/// that receives the terminal update alone is being told about a call id it
/// never saw start.
pub(super) fn assert_call_announced_before_terminal_update(notifications: &[acp::SessionNotification], call_id: &str) {
    let announced = notifications
        .iter()
        .position(|notification| {
            matches!(
                &notification.update,
                acp::SessionUpdate::ToolCall(call) if call.tool_call_id.to_string() == call_id
            )
        })
        .unwrap_or_else(|| panic!("the call {call_id} must be announced with an initial ToolCall"));
    let settled = notifications
        .iter()
        .rposition(|notification| {
            matches!(
                &notification.update,
                acp::SessionUpdate::ToolCallUpdate(update)
                    if update.tool_call_id.to_string() == call_id
                        && matches!(
                            update.fields.status,
                            Some(acp::ToolCallStatus::Completed | acp::ToolCallStatus::Failed)
                        )
            )
        })
        .expect("the call must report a terminal update");
    assert!(
        announced < settled,
        "the initial ToolCall for {call_id} must precede its terminal update: \
         announced at {announced}, settled at {settled}"
    );
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

/// Asserts the mocked tool call ran, and whether the model saw the tool offered.
///
/// The `agent` tool is advertised from the session's own subagent controller,
/// so a fail-closed session must not offer it even though the mocked model
/// emits the call regardless. Pass `advertised: false` for those cases: they
/// still require the call to reach the dispatcher and settle there, which is
/// the guarantee the fail-closed path exists to keep.
pub(super) fn assert_tool_reached_the_model(
    provider_calls: &AtomicUsize,
    provider_requests: &Mutex<Vec<LLMRequest>>,
    advertised: bool,
) {
    assert_eq!(provider_calls.load(Ordering::SeqCst), 2, "the tool result must return to the provider");
    let requests = provider_requests.lock().expect("read provider requests");
    let offered = requests[0]
        .tools
        .as_deref()
        .is_some_and(|definitions| definitions.iter().any(|tool| tool.function_name() == "agent"));
    assert_eq!(
        offered,
        advertised,
        "a session {} advertise the agent tool",
        if advertised {
            "with a subagent controller must"
        } else {
            "without a subagent controller must not"
        }
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

#[rstest]
#[tokio::test]
async fn prompt_driven_agent_tool_spawns_an_owned_background_task(
    #[future] delegation_test_guards: DelegationTestGuards,
) {
    let guards = delegation_test_guards.await;
    let provider_calls = Arc::clone(&guards.provider_calls);
    let provider_requests = Arc::clone(&guards.provider_requests);
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
            assert_tool_reached_the_model(&provider_calls, &provider_requests, true);
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
                        if let Ok(snapshot) = controller
                            .refresh_then_background_snapshot(TaskOwner::Workspace, &entry.id)
                            .await
                        {
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
                .force_cancel_background(TaskOwner::Workspace, &entry.id)
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

#[rstest]
#[tokio::test]
async fn prompt_driven_agent_tool_fails_closed_without_a_session_controller(
    #[future] delegation_test_guards: DelegationTestGuards,
) {
    use diapason_core::config::{HookCommandConfig, HookGroupConfig};

    let guards = delegation_test_guards.await;
    let provider_calls = Arc::clone(&guards.provider_calls);
    let provider_requests = Arc::clone(&guards.provider_requests);
    let workspace = TempDir::new().expect("Lody fail-closed workspace");
    write_lody_background_fixture(workspace.path());
    let hook_sentinel = workspace.path().join("pretool-hook-ran");
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.hooks.lifecycle.pre_tool_use = vec![HookGroupConfig {
        matcher: Some("agent".to_string()),
        hooks: vec![HookCommandConfig {
            // The hook would leave this file behind if it ran. Admission must
            // reject a session with no controller before the pre-tool stage.
            command: format!("cat >/dev/null; touch '{}'", hook_sentinel.display()),
            ..Default::default()
        }],
    }];
    // Build with subagent configuration so the launch registry advertises
    // `agent`, then disable subagents for session registration while keeping
    // the hook configuration, so no session controller is built and the early
    // admission rejection is the only thing that can suppress the hook.
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
    assert!(agent.local_tool_registry.has_subagent_controller());
    // Empty the launch registry's controller slot. The `agent` tool keeps its
    // snapshotted definition, but nothing can execute a spawn.
    agent.local_tool_registry = agent.local_tool_registry.with_subagent_controller(None);
    let mut session_config = vt_config.clone();
    session_config.subagents.enabled = false;
    agent.vt_config = Some(Box::new(session_config));
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
            assert_tool_reached_the_model(&provider_calls, &provider_requests, false);
            assert_tool_response_contains(&provider_requests, FAIL_CLOSED_MESSAGE);
            let notifications = tool_notifications_through_terminal(&mut updates_rx).await;
            // Admission rejects before authorization, so it must publish the
            // resolved call itself; the terminal update alone would name a
            // call id the client never saw start.
            assert_call_announced_before_terminal_update(&notifications, "call-tool");
            let update = terminal_update_in(&notifications);
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
            assert!(!hook_sentinel.exists(), "no PreToolUse hook may run when the session has no subagent controller");
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

/// A `PermissionRequest` hook may replace the arguments a call dispatches.
///
/// Admission first runs on the model's raw arguments, where a `spawn` names no
/// target and passes. The hook then rewrites the call to address a target this
/// session does not own, so admission must judge the rewritten arguments before
/// the call is marked approved or announced as in progress; otherwise only core
/// would refuse it after the ACP layer had already allowed the call.
#[rstest]
#[tokio::test]
async fn prompt_driven_agent_tool_re_admits_arguments_rewritten_by_a_permission_hook(
    #[future] delegation_test_guards: DelegationTestGuards,
) {
    use diapason_core::config::{HookCommandConfig, HookGroupConfig};

    let guards = delegation_test_guards.await;
    let provider_calls = Arc::clone(&guards.provider_calls);
    let provider_requests = Arc::clone(&guards.provider_requests);
    let workspace = TempDir::new().expect("Lody hook-rewrite workspace");
    write_lody_background_fixture(workspace.path());
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.hooks.lifecycle.permission_request = vec![HookGroupConfig {
        matcher: Some("agent".to_string()),
        hooks: vec![HookCommandConfig {
            // The mocked model emits a `spawn`, which names no target and so
            // passes the pre-hook admission check. The hook allows the call
            // but redirects it to an id the session does not own.
            command: concat!(
                "cat >/dev/null; ",
                r#"printf '{"hookSpecificOutput":{"hookEventName":"PermissionRequest","#,
                r#""decision":{"behavior":"allow"},"updatedInput":{"action":"send_input","id":"not-mine"}}}'"#
            )
            .to_string(),
            ..Default::default()
        }],
    }];
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
    assert!(
        agent.local_tool_registry.has_subagent_controller(),
        "the rewrite case needs a launch controller so session registration builds a session controller"
    );
    let agent = Arc::new(agent);
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "diapason-lody-hook-rewrite-test");
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
                .expect("the rewrite case needs a session controller so the raw spawn is admitted");
            let response = cx
                .send_request(delegation_prompt(session.session_id.clone()))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            assert_tool_reached_the_model(&provider_calls, &provider_requests, true);
            assert_tool_response_contains(&provider_requests, "not owned by this session");
            let notifications = tool_notifications_through_terminal(&mut updates_rx).await;
            assert_call_announced_before_terminal_update(&notifications, "call-tool");
            let update = terminal_update_in(&notifications);
            assert_eq!(
                update.fields.status,
                Some(acp::ToolCallStatus::Failed),
                "a rewritten target the session does not own must fail the call"
            );
            assert!(
                tool_update_text(&update).contains("not owned by this session"),
                "the client must receive the re-admission reason: {}",
                tool_update_text(&update)
            );
            assert!(
                controller.background_status_entries().await.is_empty(),
                "a rejected rewrite must not start a managed task"
            );
            close_session(&cx, session.session_id).await?;
            Ok(())
        });
    tokio::time::timeout(REQUEST_TIMEOUT, client_connection)
        .await
        .expect("Lody hook-rewrite client should finish")
        .expect("Lody hook-rewrite protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

/// A raw target this session does not own must not be rejected before an
/// approved `PermissionRequest` hook can replace it.
///
/// The model emits a `send_input` naming the pre-owner-scoping id, which this
/// session does not own. Admission must fail closed only on a missing session
/// controller at that point. The hook then allows the call and replaces the id
/// with the canonical id of a record the session does own, so the post-rewrite
/// admission admits it and the call proceeds to execution. Core reports that a
/// background record is not a child; that later rejection proves execution was
/// reached, and the terminal update must not carry the pre-hook ownership
/// rejection.
#[rstest]
#[tokio::test]
async fn prompt_driven_agent_tool_admits_a_raw_unowned_target_rewritten_to_an_owned_one(
    #[future] delegation_test_guards: DelegationTestGuards,
) {
    use diapason_core::config::{HookCommandConfig, HookGroupConfig};
    use diapason_core::subagents::SpawnBackgroundSubprocessRequest;

    let _guards = delegation_test_guards.await;
    let raw_target = r#"{"action":"send_input","id":"background-background-demo"}"#;
    let (provider_calls, provider_requests, _provider_guard) = install_agent_tool_provider_with_arguments(raw_target);
    let workspace = TempDir::new().expect("Lody raw-target rewrite workspace");
    write_lody_background_fixture(workspace.path());
    // The hook response is written once the session controller has spawned a
    // record and its canonical id is known.
    let hook_response = workspace.path().join("permission-hook-response.json");
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.hooks.lifecycle.permission_request = vec![HookGroupConfig {
        matcher: Some("agent".to_string()),
        hooks: vec![HookCommandConfig {
            command: format!("cat >/dev/null; cat '{}'", hook_response.display()),
            ..Default::default()
        }],
    }];
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
    assert!(
        agent.local_tool_registry.has_subagent_controller(),
        "the rewrite case needs a launch controller so session registration builds a session controller"
    );
    let agent = Arc::new(agent);
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "diapason-lody-raw-target-rewrite-test");
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
                .expect("the raw target must not be rejected when a session controller exists");
            let owned = controller
                .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
                    agent_type: Some("background-demo".to_string()),
                    ..SpawnBackgroundSubprocessRequest::default()
                })
                .await
                .expect("spawn a background record this session owns");
            assert_ne!(
                owned.id, "background-background-demo",
                "the fixture must have an owner-scoped canonical id for this regression"
            );
            let response = serde_json::json!({
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" },
                    "updatedInput": {
                        "action": "send_input",
                        "id": owned.id.clone(),
                        "message": "continue",
                    },
                }
            });
            std::fs::write(&hook_response, response.to_string()).expect("write the permission hook response");

            let response = cx
                .send_request(delegation_prompt(session.session_id.clone()))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            assert_tool_reached_the_model(&provider_calls, &provider_requests, true);
            // The call reached the core executor, which reports that a
            // background record is not a child. That is a later layer; the
            // pre-hook ownership rejection must not appear.
            assert_tool_response_contains(&provider_requests, "Unknown subagent id");
            let notifications = tool_notifications_through_terminal(&mut updates_rx).await;
            assert_call_announced_before_terminal_update(&notifications, "call-tool");
            let update = terminal_update_in(&notifications);
            let text = tool_update_text(&update);
            assert!(text.contains("Unknown subagent id"), "the rewritten call must reach execution: {text}");
            assert!(
                !text.contains("not owned by this session"),
                "an approved rewrite to an owned target must not be rejected for ownership: {text}"
            );
            drop(
                controller
                    .force_cancel_background(TaskOwner::Workspace, &owned.id)
                    .await
                    .expect("cancel the owned background task during cleanup"),
            );
            close_session(&cx, session.session_id).await?;
            Ok(())
        });
    tokio::time::timeout(REQUEST_TIMEOUT, client_connection)
        .await
        .expect("Lody raw-target rewrite client should finish")
        .expect("Lody raw-target rewrite protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
