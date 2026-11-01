//! Nested delegation ownership regressions for the Lody management extensions.
use super::lody_support::{
    GlobalProviderRegistration, NESTED_CHILD_TASK, NESTED_GRANDCHILD_TASK, NESTED_MODEL, NESTED_PROVIDER,
    ParkedProvider, lody_cancel, lody_list, lody_output,
};
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use std::time::Duration;
use vtcode_core::subagents::SpawnAgentRequest;

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
            let listed = lody_list(&cx, &session.session_id, false).await?;
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
            let foreign_list = lody_list(&cx, &foreign_session.session_id, false).await?;
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
