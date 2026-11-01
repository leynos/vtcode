//! Exercises the Lody subagent-management extensions over the official ACP duplex.

use super::lody_support::{
    lifecycle_task_id, lifecycle_task_meta, lody_list, lody_output_error, write_lody_background_fixture,
};
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use std::time::Duration;
use vtcode_core::subagents::SpawnBackgroundSubprocessRequest;

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
