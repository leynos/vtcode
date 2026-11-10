//! Proves fail-closed delegation when the workspace shares a controller but
//! the prompting session does not own one.
//!
//! The sibling `lody_prompt_delegation` case clears the launch registry
//! controller itself, so it cannot show that an absent *session* controller
//! fails closed while a shared workspace controller remains able to spawn.

use super::lody_prompt_delegation::{
    FAIL_CLOSED_MESSAGE, assert_tool_reached_the_model, assert_tool_response_contains, delegation_prompt,
    install_agent_tool_provider, terminal_tool_update, tool_update_text,
};
use super::lody_support::{
    background_ledger_path, close_session, open_build_session, spawn_agent_connection, write_lody_background_fixture,
};
use super::*;
use std::time::Duration;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[tokio::test]
async fn prompt_driven_agent_tool_fails_closed_when_only_the_workspace_has_a_controller() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let (provider_calls, provider_requests, _factory_guard) = install_agent_tool_provider();
    let workspace = TempDir::new().expect("Lody shared-controller workspace");
    write_lody_background_fixture(workspace.path());
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    // Build with subagent configuration so the `agent` tool definition is
    // snapshotted, then attach the workspace controller directly, exactly as
    // the management tests do.
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&vt_config), None).await;
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
    assert!(agent.local_tool_registry.has_subagent_controller());
    // Per-session attachment reads the agent's runtime config. Clearing it
    // leaves the session without a controller while the launch workspace
    // registry, which the session workspace runtime shares, keeps its own.
    agent.vt_config = None;
    let agent = Arc::new(agent);
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "diapason-lody-prompt-shared-controller-test");
    let inspected_agent = Arc::clone(&agent);
    let workspace_path = workspace.path().to_path_buf();
    let ledger_path = background_ledger_path(&workspace_path);
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
            let runtime = owned_session
                .workspace_runtime()
                .expect("read the session runtime")
                .expect("the session must have a workspace runtime");
            assert!(
                Arc::ptr_eq(&runtime, &inspected_agent.launch_workspace_runtime),
                "the session must share the launch workspace runtime"
            );
            assert!(
                inspected_agent
                    .session_subagent_controller(&owned_session)
                    .expect("resolve the session controller")
                    .is_none(),
                "the session must not own a subagent controller"
            );
            assert!(
                runtime.local_tool_registry.has_subagent_controller(),
                "the shared workspace registry must keep the launch controller"
            );
            let controller = runtime
                .local_tool_registry
                .subagent_controller()
                .expect("shared workspace controller");
            assert!(
                controller.background_status_entries().await.is_empty(),
                "the shared controller must start with no managed task"
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
                "a session without a controller must fail the tool call"
            );
            assert!(
                tool_update_text(&update).contains(FAIL_CLOSED_MESSAGE),
                "the client must receive the fail-closed message: {}",
                tool_update_text(&update)
            );
            let entries = controller.background_status_entries().await;
            assert!(entries.is_empty(), "fail-closed delegation must not record a shared background task: {entries:?}");
            assert!(
                entries.iter().all(|entry| entry.pid.is_none()),
                "fail-closed delegation must not start a child process"
            );
            assert!(!ledger_path.exists(), "fail-closed delegation must not persist a background ledger");
            close_session(&cx, session.session_id).await?;
            Ok(())
        });
    tokio::time::timeout(REQUEST_TIMEOUT, client_connection)
        .await
        .expect("Lody shared-controller client should finish")
        .expect("Lody shared-controller protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
