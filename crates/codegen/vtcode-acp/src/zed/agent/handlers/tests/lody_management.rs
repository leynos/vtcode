//! Exercises the Lody subagent-management extensions over the official ACP duplex.

use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use std::time::Duration;
use vtcode_core::subagents::{SpawnAgentRequest, SpawnBackgroundSubprocessRequest};

#[test]
fn advertised_capabilities_include_lody_subagent_management_with_subagents() {
    let capabilities = advertised_agent_capabilities(true, false);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert_eq!(lody["subagents"]["version"], 1);
    assert_eq!(lody["subagents"]["lifecycle"], true);
    assert_eq!(lody["subagents"]["list"], true);
    assert_eq!(lody["subagents"]["cancel"], true);
    assert_eq!(lody["subagents"]["output"], true);
    assert!(lody.get("tasks").is_none());
    assert_eq!(lody["usage"]["version"], 1);
}

#[test]
fn advertised_capabilities_include_background_tasks_only_when_enabled() {
    let capabilities = advertised_agent_capabilities(true, true);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert_eq!(lody["tasks"]["version"], 1);
    assert_eq!(lody["tasks"]["background"], true);
}

#[tokio::test]
async fn lody_subagent_management_extensions_round_trip_over_official_acp_duplex() {
    let workspace = TempDir::new().expect("Lody management workspace");
    write_lody_background_fixture(workspace.path());
    let launch_workspace = TempDir::new().expect("separate launch workspace");
    let mut agent = build_wire_test_agent(launch_workspace.path()).await;
    let mut vt_config = VTCodeConfig::default();
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
        },
    )
    .await;
    assert_eq!(setup_outcome, crate::zed::agent::OptionalSetupOutcome::Attached);
    let agent = Arc::new(agent);
    assert!(agent.local_tool_registry.has_subagent_controller());
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-lody-management-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let workspace_path = workspace.path().to_path_buf();
    let inspected_agent = Arc::clone(&agent);
    let (notification_tx, mut notification_rx) = mpsc::unbounded_channel();
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(notification_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            let initialized = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            let lody = &initialized.agent_capabilities.meta.expect("Lody capabilities")["lody"];
            assert_eq!(lody["subagents"]["list"], true);
            assert_eq!(lody["tasks"]["background"], true);

            let session = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;
            let owned_session = inspected_agent
                .session_handle(&session.session_id)
                .expect("look up the session")
                .expect("registered session");
            let controller = inspected_agent
                .session_subagent_controller(&owned_session)
                .expect("resolve the session controller")
                .expect("session has a subagent controller");
            // Session creation installs a forwarder. Detach it first so the task
            // starts with no observer registered, which is the state a resumed
            // session restores into.
            owned_session
                .stop_task_lifecycle_forwarder()
                .await
                .expect("stop lifecycle forwarder before spawning");
            let background = controller
                .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
                    agent_type: Some("background-demo".to_string()),
                    ..SpawnBackgroundSubprocessRequest::default()
                })
                .await
                .expect("spawn owned background task");
            while let Ok(notification) = notification_rx.try_recv() {
                assert!(
                    lifecycle_task_id(&notification).is_none(),
                    "no lifecycle notification may be forwarded before the forwarder attaches"
                );
            }

            // Attaching the lifecycle forwarder after the task is already
            // running must publish its current snapshot; otherwise a resumed
            // task that never transitions again stays invisible to the client.
            inspected_agent
                .ensure_task_lifecycle_forwarder(&owned_session)
                .expect("start lifecycle forwarder for the attached session");
            let initial_task = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let notification = notification_rx.recv().await.expect("lifecycle notification stream");
                    if let Some(task) = lifecycle_task_meta(&notification) {
                        break task;
                    }
                }
            })
            .await
            .expect("initial lifecycle snapshot deadline");
            assert_eq!(
                initial_task["taskId"], background.id,
                "a task that was already running must be published when the forwarder attaches"
            );
            assert_eq!(initial_task["kind"], "background");
            let restored_status = initial_task["status"].as_str().expect("projected task status");
            assert!(
                matches!(restored_status, "pending" | "in_progress"),
                "a restored task that is still running must be published as active, got {restored_status}"
            );

            let response = lody_list(&cx, &session.session_id, false).await?;
            assert_eq!(response["tasks"][0]["taskId"], background.id);

            let mut output = serde_json::Value::Null;
            for _ in 0..20 {
                let params = serde_json::value::to_raw_value(&serde_json::json!({
                    "sessionId": session.session_id,
                    "taskId": background.id,
                    "tail": 1,
                }))?;
                output = cx
                    .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
                        params.into(),
                    )))
                    .block_task()
                    .await?;
                if output["output"] == "lody-output-two" {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            assert_eq!(output["output"], "lody-output-two");

            let foreign_session = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;
            let foreign_list = lody_list(&cx, &foreign_session.session_id, false).await?;
            assert_eq!(foreign_list, serde_json::json!({ "tasks": [] }));

            let foreign_owned_session = inspected_agent
                .session_handle(&foreign_session.session_id)
                .expect("look up the foreign session")
                .expect("registered foreign session");
            let foreign_controller = inspected_agent
                .session_subagent_controller(&foreign_owned_session)
                .expect("resolve the foreign session controller")
                .expect("foreign session has a subagent controller");
            let first_runtime = owned_session
                .workspace_runtime()
                .expect("read the session runtime")
                .expect("session has a workspace runtime");
            let foreign_runtime = foreign_owned_session
                .workspace_runtime()
                .expect("read the foreign session runtime")
                .expect("foreign session has a workspace runtime");
            assert!(
                Arc::ptr_eq(&first_runtime, &foreign_runtime),
                "session ownership must preserve shared workspace resources"
            );
            assert!(
                !Arc::ptr_eq(&controller, &foreign_controller),
                "controllers must retain independent immutable owners"
            );
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": foreign_session.session_id, "taskId": background.id,
            }))?;
            let foreign_cancel = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
                    params.into(),
                )))
                .block_task()
                .await
                .expect_err("a sibling must not cancel the owned task");
            assert_eq!(foreign_cancel.data, Some(serde_json::json!({ "reason": "unknown_task" })));
            assert!(
                controller
                    .background_snapshot(&background.id)
                    .await
                    .expect("read the owned background snapshot")
                    .entry
                    .status
                    .is_active()
            );

            let foreign_error = lody_output_error(&cx, &foreign_session.session_id, &background.id).await;
            let unknown_error = lody_output_error(&cx, &session.session_id, "unknown-task").await;
            assert_eq!(foreign_error, unknown_error);
            assert_eq!(foreign_error.data, Some(serde_json::json!({ "reason": "unknown_task" })));

            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": session.session_id,
                "taskId": background.id,
                "reason": "test complete",
            }))?;
            let cancelled = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(cancelled, serde_json::json!({}));

            // Exercise the registry route itself: a detached session view must
            // never execute through the shared runtime's ownerless controller.
            let registry = foreign_runtime
                .local_tool_registry
                .with_subagent_controller(Some(Arc::clone(&foreign_controller)));
            registry.mark_tool_preapproved("agent").await;
            let spawned = registry
                .execute_tool(
                    "agent",
                    serde_json::json!({
                        "action": "spawn_subprocess", "agent_type": "background-demo",
                    }),
                )
                .await
                .expect("spawn through the session registry executor");
            let sibling = foreign_controller
                .background_status_entries()
                .await
                .into_iter()
                .next()
                .expect("sibling task");
            assert_eq!(sibling.owner_session_id.as_deref(), Some(foreign_session.session_id.0.as_ref()));
            assert_ne!(
                sibling.exec_session_id, background.exec_session_id,
                "same-workspace sessions need distinct child processes"
            );
            assert_eq!(spawned["id"], sibling.id);
            let disabled = registry.with_subagent_controller(None);
            // Preapproval is consumed once, including on the successful spawn.
            disabled.mark_tool_preapproved("agent").await;
            let denied = disabled
                .execute_tool(
                    "agent",
                    serde_json::json!({
                        "action": "spawn_subprocess", "agent_type": "background-demo",
                    }),
                )
                .await
                .expect("registry execution errors are returned in the public JSON envelope");
            assert_eq!(denied["error"]["message"], "Subagent controller is not active in this session");
            assert_eq!(denied["error"]["category"], "ExecutionError");
            assert!(denied.get("id").is_none(), "an empty controller slot must not spawn a child: {denied}");
            assert!(registry.has_subagent_controller(), "detaching a sibling view must not clear its controller");

            drop(
                cx.send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert!(
                foreign_controller
                    .background_snapshot(&sibling.id)
                    .await
                    .expect("read the sibling background snapshot")
                    .entry
                    .status
                    .is_active(),
                "closing the first session must not stop a sibling's live child process"
            );
            assert_eq!(
                first_runtime
                    .lifecycle
                    .lock()
                    .expect("workspace runtime lifecycle lock")
                    .active_sessions,
                1
            );
            assert_eq!(
                first_runtime.lifecycle.lock().expect("workspace runtime lifecycle lock").state,
                crate::zed::agent::session_workspace::WorkspaceRuntimeLifecycleState::Open
            );
            let cancelled_sibling = foreign_controller
                .force_cancel_background(&sibling.id)
                .await
                .expect("cancel the sibling background task");
            assert!(!cancelled_sibling.status.is_active(), "the sibling process must be stopped during test cleanup");
            drop(
                cx.send_request(CloseSessionRequest::new(foreign_session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert_eq!(
                first_runtime
                    .lifecycle
                    .lock()
                    .expect("workspace runtime lifecycle lock")
                    .active_sessions,
                0
            );
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("Lody management client should finish")
        .expect("Lody management protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

// ─── Nested delegation over ACP ─────────────────────────────────────────────

/// Provider used by the child and grandchild runners in the nested-management
/// regression.
///
/// Child runners resolve providers through the process-global
/// `create_provider_with_config`, not through the ACP prompt override, so the
/// mock has to be registered in the global factory for the duration of the
/// test.
const NESTED_PROVIDER: &str = "nested-delegation-mock";
const NESTED_MODEL: &str = vtcode_config::constants::models::anthropic::CLAUDE_SONNET_5;
/// Task handed to the `default` child.
const NESTED_CHILD_TASK: &str = "Survey the delegated fixture and report readiness.";
/// Task the child delegates to its read-only grandchild.
const NESTED_GRANDCHILD_TASK: &str = "grandchild-marker: list every source file and report each path.";

/// Removes the mock provider from the process-global factory on drop.
struct GlobalProviderRegistration(&'static str);

impl Drop for GlobalProviderRegistration {
    fn drop(&mut self) {
        if let Ok(mut factory) = vtcode_core::llm::factory::get_factory().lock() {
            factory.remove_provider(self.0);
        }
    }
}

/// Provider that never answers, parking every child run it serves.
///
/// Parking instead of completing keeps the nested tree alive and observable
/// while the test inspects it; the ACP cancel aborts the parked futures.
struct ParkedProvider;

#[async_trait]
impl LLMProvider for ParkedProvider {
    fn name(&self) -> &str {
        NESTED_PROVIDER
    }

    fn supported_models(&self) -> Vec<String> {
        vec![NESTED_MODEL.to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        std::future::pending().await
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        std::future::pending().await
    }
}

async fn lody_list(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    active_only: bool,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "activeOnly": active_only,
    }))
    .expect("serialize Lody list request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

async fn lody_output(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
        "tail": 20,
    }))
    .expect("serialize Lody output request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

async fn lody_cancel(
    connection: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    task_id: &str,
) -> Result<serde_json::Value, acp::Error> {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
        "reason": "nested management regression",
    }))
    .expect("serialize Lody cancel request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
            params.into(),
        )))
        .block_task()
        .await
}

/// A session's management surface must reach grandchildren spawned by its own
/// children, and must not reach a sibling session's tree.
///
/// The root controller tracks the `default` child; that child's scoped
/// controller tracks the `explorer` grandchild. Ownership is derived from the
/// parent/child session chain, so the sibling sees neither task even though it
/// shares the same workspace runtime.
#[tokio::test]
async fn nested_subagent_management_is_owned_by_the_root_session() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    vtcode_core::llm::factory::get_factory()
        .lock()
        .expect("global LLM factory")
        .register_provider(NESTED_PROVIDER, |_config| Box::new(ParkedProvider));
    let _provider_registration = GlobalProviderRegistration(NESTED_PROVIDER);

    let launch_workspace = TempDir::new().expect("separate launch workspace");
    let session_workspace = TempDir::new().expect("nested delegation workspace");
    let mut agent = build_wire_test_agent(launch_workspace.path()).await;
    // Child runners parse the parent model, so it must be a real model id.
    agent.config.model = NESTED_MODEL.to_string();
    agent.config.provider = NESTED_PROVIDER.to_string();
    let mut vt_config = VTCodeConfig::default();
    vt_config.subagents.enabled = true;
    // The child needs room to delegate one level deeper.
    vt_config.subagents.max_depth = 2;
    vt_config.agent.provider = NESTED_PROVIDER.to_string();
    vt_config.agent.default_model = NESTED_MODEL.to_string();
    vt_config.agent.small_model.model = NESTED_MODEL.to_string();
    // Delegation is the behaviour under test, not planner/generator/evaluator
    // orchestration: keep the child on the single-pass loop so it never issues
    // a tool-less planner request against the parked provider.
    vt_config.agent.harness.orchestration_mode = vtcode_config::core::agent::HarnessOrchestrationMode::Single;
    agent.vt_config = Some(Box::new(vt_config));
    let agent = Arc::new(agent);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-nested-delegation-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let session_workspace_path = session_workspace.path().to_path_buf();
    let inspected_agent = Arc::clone(&agent);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx
                .send_request(NewSessionRequest::new(session_workspace_path.clone()))
                .block_task()
                .await?;
            let owned_session = inspected_agent
                .session_handle(&session.session_id)
                .expect("look up the session")
                .expect("registered session");
            let root = inspected_agent
                .session_subagent_controller(&owned_session)
                .expect("resolve the root controller")
                .expect("session has a subagent controller");

            let child = root
                .spawn(SpawnAgentRequest {
                    agent_type: Some("default".to_string()),
                    message: Some(NESTED_CHILD_TASK.to_string()),
                    ..SpawnAgentRequest::default()
                })
                .await
                .expect("spawn the default child");
            assert_eq!(child.agent_name, "default");
            assert_eq!(child.depth, 1, "a direct child of the root session sits at depth 1");

            // The child's scoped controller is the seam its own `agent` tool
            // uses to delegate. It is attached before the child's first model
            // turn, so poll briefly for it, then spawn the grandchild through
            // it: the tree shape must not depend on which tool profile the
            // child runner happens to expose to its model.
            let polled = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Some(controller) = root.descendant_controllers().await.into_iter().next() {
                        return controller;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            let child_controller = match polled {
                Ok(controller) => controller,
                Err(_) => {
                    let entries = root.status_entries().await;
                    panic!("the default child must expose a child-scoped controller (child entries={entries:?})");
                }
            };
            let grandchild = child_controller
                .spawn(SpawnAgentRequest {
                    agent_type: Some("explorer".to_string()),
                    message: Some(NESTED_GRANDCHILD_TASK.to_string()),
                    ..SpawnAgentRequest::default()
                })
                .await
                .expect("spawn the explorer grandchild through the child's controller");
            assert_eq!(grandchild.agent_name, "explorer");
            assert_eq!(grandchild.parent_thread_id, child.session_id);
            assert_eq!(grandchild.depth, 2, "a grandchild of the root session sits at depth 2");

            // A second grandchild keeps a live witness for the subtree-close
            // assertion after the first one is cancelled directly.
            let second_grandchild = child_controller
                .spawn(SpawnAgentRequest {
                    agent_type: Some("explorer".to_string()),
                    message: Some(NESTED_GRANDCHILD_TASK.to_string()),
                    ..SpawnAgentRequest::default()
                })
                .await
                .expect("spawn a second explorer grandchild through the child's controller");
            assert_eq!(second_grandchild.parent_thread_id, child.session_id);
            assert_eq!(second_grandchild.depth, 2);

            // The owning session sees the whole tree.
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": session.session_id,
                "activeOnly": false,
            }))?;
            let listed = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            let tasks = listed["tasks"].as_array().expect("Lody task list").clone();
            let mut listed_ids = tasks.iter().filter_map(|task| task["taskId"].as_str()).collect::<Vec<_>>();
            listed_ids.sort_unstable();
            let mut expected_ids = vec![child.id.as_str(), grandchild.id.as_str(), second_grandchild.id.as_str()];
            expected_ids.sort_unstable();
            assert_eq!(
                listed_ids, expected_ids,
                "the root session must list its child and both grandchildren exactly once: {listed}"
            );

            // A sibling session shares the workspace runtime but owns nothing.
            let foreign_session = cx
                .send_request(NewSessionRequest::new(session_workspace_path.clone()))
                .block_task()
                .await?;
            let params = serde_json::value::to_raw_value(&serde_json::json!({
                "sessionId": foreign_session.session_id,
                "activeOnly": false,
            }))?;
            let foreign_list = cx
                .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
                    crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                    params.into(),
                )))
                .block_task()
                .await?;
            assert_eq!(foreign_list, serde_json::json!({ "tasks": [] }));

            // A foreign task must be indistinguishable from an unknown one on
            // both management methods, so ownership is not an oracle.
            let foreign_output = lody_output(&cx, &foreign_session.session_id, &grandchild.id)
                .await
                .expect_err("a sibling must not read a grandchild's output");
            let unknown_output = lody_output(&cx, &session.session_id, "task-that-does-not-exist")
                .await
                .expect_err("an unknown task ID must not resolve");
            assert_eq!(
                foreign_output, unknown_output,
                "a foreign output request must return the same error value as an unknown one"
            );
            assert_eq!(foreign_output.data, Some(serde_json::json!({ "reason": "unknown_task" })));

            let foreign_cancel = lody_cancel(&cx, &foreign_session.session_id, &grandchild.id)
                .await
                .expect_err("a sibling must not cancel a grandchild");
            let unknown_cancel = lody_cancel(&cx, &session.session_id, "task-that-does-not-exist")
                .await
                .expect_err("cancelling an unknown task must fail");
            assert_eq!(
                foreign_cancel, unknown_cancel,
                "a foreign cancel must return the same error value as an unknown one"
            );
            assert_eq!(foreign_cancel.data, Some(serde_json::json!({ "reason": "unknown_task" })));
            assert!(
                !child_controller
                    .status_entries()
                    .await
                    .into_iter()
                    .find(|entry| entry.id == grandchild.id)
                    .expect("grandchild record")
                    .status
                    .is_terminal(),
                "a rejected sibling cancel must not stop the grandchild"
            );

            // The owner can read the grandchild through the child's scoped
            // controller. The runtime snapshot materialises once the child run
            // has recorded its configuration, so poll briefly.
            let owned_output = tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    if let Ok(output) = lody_output(&cx, &session.session_id, &grandchild.id).await {
                        break output;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("the owning session must read the grandchild's output");
            assert!(
                owned_output.get("output").is_some(),
                "grandchild output must use the managed output envelope: {owned_output}"
            );

            // The owner cancels one grandchild directly: management must route
            // through the child-scoped controller that owns it, not merely
            // through the root the task was listed from.
            let cancelled_grandchild = lody_cancel(&cx, &session.session_id, &grandchild.id)
                .await
                .expect("the owning session must cancel an owned grandchild");
            assert_eq!(cancelled_grandchild, serde_json::json!({}));
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let terminal = child_controller
                        .status_entries()
                        .await
                        .into_iter()
                        .find(|entry| entry.id == grandchild.id)
                        .is_some_and(|entry| entry.status.is_terminal());
                    if terminal {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("cancelling an owned grandchild must settle it directly");

            let sibling_still_running = child_controller
                .status_entries()
                .await
                .into_iter()
                .find(|entry| entry.id == second_grandchild.id)
                .is_some_and(|entry| !entry.status.is_terminal());
            assert!(sibling_still_running, "cancelling one grandchild must not cancel its sibling");
            let child_still_running = root
                .status_entries()
                .await
                .into_iter()
                .find(|entry| entry.id == child.id)
                .is_some_and(|entry| !entry.status.is_terminal());
            assert!(child_still_running, "cancelling a grandchild must not cancel its parent");

            // Cancelling the child closes the rest of its subtree.
            let cancelled = lody_cancel(&cx, &session.session_id, &child.id)
                .await
                .expect("the owning session must cancel its child");
            assert_eq!(cancelled, serde_json::json!({}));
            tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let terminal = child_controller
                        .status_entries()
                        .await
                        .into_iter()
                        .find(|entry| entry.id == second_grandchild.id)
                        .is_some_and(|entry| entry.status.is_terminal());
                    if terminal {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("closing the child must close its remaining grandchild subtree");

            drop(
                cx.send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            drop(
                cx.send_request(CloseSessionRequest::new(foreign_session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(60), client_connection)
        .await
        .expect("nested delegation client should finish")
        .expect("nested delegation protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}

/// Extracts the Lody task metadata from a task-lifecycle session notification.
fn lifecycle_task_meta(notification: &acp::AgentNotification) -> Option<serde_json::Value> {
    let acp::AgentNotification::SessionNotification(notification) = notification else {
        return None;
    };
    let meta = match &notification.update {
        acp::SessionUpdate::ToolCall(call) => call.meta.as_ref(),
        acp::SessionUpdate::ToolCallUpdate(update) => update.meta.as_ref(),
        _ => None,
    }?;
    let task = &meta["lody"]["task"];
    task.get("taskId")?.as_str().map(|_| task.clone())
}

/// Extracts the Lody task ID from a task-lifecycle session notification.
fn lifecycle_task_id(notification: &acp::AgentNotification) -> Option<String> {
    lifecycle_task_meta(notification)?.get("taskId")?.as_str().map(str::to_owned)
}

async fn lody_output_error(connection: &ConnectionTo<Agent>, session_id: &acp::SessionId, task_id: &str) -> acp::Error {
    let params = serde_json::value::to_raw_value(&serde_json::json!({
        "sessionId": session_id,
        "taskId": task_id,
    }))
    .expect("serialize Lody output request");
    connection
        .send_request(acp::ClientRequest::ExtMethodRequest(acp::ExtRequest::new(
            crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
            params.into(),
        )))
        .block_task()
        .await
        .expect_err("Lody output request should fail")
}

fn write_lody_background_fixture(workspace: &std::path::Path) {
    let agent_dir = workspace.join(".vtcode/agents");
    std::fs::create_dir_all(&agent_dir).expect("create Lody test agent directory");
    std::fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Lody management protocol fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write Lody test agent");
    let scripts_dir = workspace.join("scripts");
    std::fs::create_dir_all(&scripts_dir).expect("create Lody test scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    std::fs::write(&script, "#!/bin/sh\nprintf 'lody-output-one\\nlody-output-two\\n'\nsleep 30\n")
        .expect("write Lody background script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&script)
            .expect("Lody background script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(script, permissions).expect("make Lody background script executable");
    }
}
