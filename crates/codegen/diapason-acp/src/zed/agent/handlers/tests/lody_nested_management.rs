//! Nested delegation ownership regressions for the Lody management extensions.
use super::lody_management_assertions::{
    assert_closing_child_closes_its_subtree, assert_owned_grandchild_cancel_settles_only_it,
    assert_sibling_cannot_reach_the_tree,
};
use super::lody_management_projection::assert_owned_tree_is_listed;
use super::lody_support::{
    GlobalProviderRegistration, NESTED_CHILD_TASK, NESTED_GRANDCHILD_TASK, NESTED_MODEL, NESTED_PROVIDER,
    ParkedProvider,
};
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use diapason_core::subagents::SpawnAgentRequest;
use std::time::Duration;

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

    diapason_core::llm::factory::get_factory()
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
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    // The child needs room to delegate one level deeper.
    vt_config.subagents.max_depth = 2;
    vt_config.agent.provider = NESTED_PROVIDER.to_string();
    vt_config.agent.default_model = NESTED_MODEL.to_string();
    vt_config.agent.small_model.model = NESTED_MODEL.to_string();
    // Delegation is the behaviour under test, not planner/generator/evaluator
    // orchestration: keep the child on the single-pass loop so it never issues
    // a tool-less planner request against the parked provider.
    vt_config.agent.harness.orchestration_mode = diapason_config::core::agent::HarnessOrchestrationMode::Single;
    agent.vt_config = Some(Box::new(vt_config));
    let agent = Arc::new(agent);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(
        Agent.builder().name("diapason-nested-delegation-test"),
        Arc::clone(&agent),
    )
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
            assert_owned_tree_is_listed(
                &cx,
                &session.session_id,
                &root,
                &child_controller,
                &child.id,
                [&grandchild.id, &second_grandchild.id],
            )
            .await?;

            // A sibling session shares the workspace runtime but owns nothing.
            let foreign_session = cx
                .send_request(NewSessionRequest::new(session_workspace_path.clone()))
                .block_task()
                .await?;
            assert_sibling_cannot_reach_the_tree(
                &cx,
                &session.session_id,
                &foreign_session.session_id,
                &grandchild.id,
                &child_controller,
            )
            .await?;

            // The owner cancels one grandchild directly, which must settle that
            // record alone.
            assert_owned_grandchild_cancel_settles_only_it(
                &cx,
                &session.session_id,
                &root,
                &child_controller,
                &child.id,
                &grandchild.id,
                &second_grandchild.id,
            )
            .await?;

            // Cancelling the child closes the rest of its subtree.
            assert_closing_child_closes_its_subtree(
                &cx,
                &session.session_id,
                &child_controller,
                &child.id,
                &second_grandchild.id,
            )
            .await?;

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
