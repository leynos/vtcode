//! Exercises the Lody subagent-management extensions over the official ACP duplex.
//!
//! The lifecycle publication and list projections live in
//! `lody_management_projection`, the read-boundary probes in
//! `lody_management_reads`, and the registry and metrics assertions in
//! `lody_management_assertions`, so this module keeps only the protocol flow
//! that sequences them.

use super::lody_management_assertions::{
    ManagementRequestTotals, assert_management_requests_are_accounted, cancel_background_task,
    exercise_detached_session_registry,
};
use super::lody_management_projection::{assert_active_task_projection, publish_forwarder_snapshot_on_attach};
use super::lody_management_reads::{
    read_exited_output_without_a_recorded_archive_is_inert, read_live_output_until_recorded,
    read_stale_record_output_is_inert, spawn_stale_background_record,
};
use super::lody_support::{
    LODY_BACKGROUND_SCRIPT, expected_epoch_seconds, lifecycle_task_id, lody_cancel_with_reason, lody_ext, lody_list,
    lody_output_error, lody_output_tail, runtime_active_sessions, write_lody_background_fixture,
    write_lody_background_script,
};
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use diapason_core::subagents::{SpawnBackgroundSubprocessRequest, TaskOwner, background_record_id};
use std::time::Duration;

#[tokio::test]
async fn lody_subagent_management_extensions_round_trip_over_official_acp_duplex() {
    let workspace = TempDir::new().expect("Lody management workspace");
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
            owner_migration_failed: false,
        },
    )
    .await;
    assert_eq!(setup_outcome, crate::zed::agent::OptionalSetupOutcome::Attached);
    let agent = Arc::new(agent);
    assert!(agent.local_tool_registry.has_subagent_controller());
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("diapason-lody-management-test"), Arc::clone(&agent))
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

            publish_forwarder_snapshot_on_attach(
                &inspected_agent,
                &owned_session,
                &background.id,
                &mut notification_rx,
            )
            .await;
            let active_started_at_epoch_seconds =
                assert_active_task_projection(&cx, &controller, &session.session_id, &background.id).await;

            let (output, live_output_polls) = read_live_output_until_recorded(
                &cx,
                &controller,
                &session.session_id,
                &background.id,
                &workspace_path,
                &mut notification_rx,
            )
            .await?;
            assert_eq!(
                output["output"], "lody-output-two",
                "the live output read must return the fixture's final line: {output}"
            );

            // A client may still hold the pre-owner-scoping id. The management
            // layer resolves it to the canonical record the session owns, so
            // the read lands on the same task rather than failing as unknown.
            let legacy_id = background_record_id("background-demo");
            assert_ne!(
                legacy_id, background.id,
                "the fixture must exercise an owner-scoped canonical id for this regression"
            );
            let legacy_output = lody_output_tail(&cx, &session.session_id, &legacy_id, 1).await?;
            assert_eq!(
                legacy_output["output"], "lody-output-two",
                "a legacy id must resolve to the owning session's record: {legacy_output}"
            );

            let oversized_tail = lody_ext(
                &cx,
                crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
                serde_json::json!({
                    "sessionId": session.session_id,
                    "taskId": background.id,
                    "tail": 10_001,
                }),
            )
            .await
            .expect_err("a tail above the transport bound must be rejected");
            assert_eq!(
                oversized_tail.data.as_ref().and_then(|data| data.get("reason")),
                Some(&serde_json::json!("invalid_tail")),
                "an oversized tail must report the bounded invalid_tail reason: {oversized_tail:?}"
            );

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
            let foreign_cancel =
                lody_cancel_with_reason(&cx, &foreign_session.session_id, &background.id, "denied sibling")
                    .await
                    .expect_err("a sibling must not cancel the owned task");
            assert_eq!(foreign_cancel.data, Some(serde_json::json!({ "reason": "unknown_task" })));
            assert!(
                controller
                    .refresh_then_background_snapshot(TaskOwner::Workspace, &background.id)
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
            let foreign_legacy_error = lody_output_error(&cx, &foreign_session.session_id, &legacy_id).await;
            assert_eq!(
                foreign_legacy_error.data,
                Some(serde_json::json!({ "reason": "unknown_task" })),
                "a legacy id must not widen ownership to another session: {foreign_legacy_error:?}"
            );

            // Cancelling by the legacy id resolves to the same canonical
            // record, so a client that retained the old id can still stop it.
            let cancelled = lody_cancel_with_reason(&cx, &session.session_id, &legacy_id, "test complete").await?;
            assert_eq!(cancelled, serde_json::json!({}));

            // The cancelled task records its end asynchronously; wait for the
            // controller to observe it before asserting the terminal projection.
            let ended_entry = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let entry = controller
                        .refresh_then_background_snapshot(TaskOwner::Workspace, &background.id)
                        .await
                        .expect("read the owned background snapshot")
                        .entry;
                    if entry.ended_at.is_some() {
                        break entry;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .expect("the cancelled background task must record its end");

            // A cancelled task is terminal: the unfiltered list still reports
            // it, while `activeOnly` must drop it.
            let all_after_cancel = lody_list(&cx, &session.session_id, false).await?;
            assert_eq!(all_after_cancel["tasks"][0]["taskId"], background.id);
            assert_eq!(
                all_after_cancel["tasks"][0]["status"], "completed",
                "a cancelled background task is reported as a completed task: {all_after_cancel}"
            );
            assert_eq!(
                all_after_cancel["tasks"][0]["startedAtEpochSeconds"], active_started_at_epoch_seconds,
                "cancelling a task must not move its start instant: {all_after_cancel}"
            );
            assert_eq!(
                all_after_cancel["tasks"][0]["endedAtEpochSeconds"],
                expected_epoch_seconds(ended_entry.ended_at.expect("the cancelled task records an end instant")),
                "the listed end must project the controller record: {all_after_cancel}"
            );
            let active_after_cancel = lody_list(&cx, &session.session_id, true).await?;
            assert_eq!(
                active_after_cancel,
                serde_json::json!({ "tasks": [] }),
                "activeOnly must exclude a terminal task"
            );

            let _exited_output = read_exited_output_without_a_recorded_archive_is_inert(
                &cx,
                &controller,
                &session.session_id,
                &background.id,
                &workspace_path,
                &mut notification_rx,
            )
            .await?;

            // A record the controller still caches as active, whose child has
            // already exited, is the case an accidental refresh would settle.
            // The read must not refresh, settle, respawn, persist, or publish
            // anything. The missing-exec-session arm of the same boundary is
            // covered by the reader's own `missing_exec_session_falls_back_to_the_archive`
            // and `not_ready_live_output_falls_back_to_the_archive` cases.
            let stale = spawn_stale_background_record(&controller, &session.session_id, &workspace_path).await;
            let _stale_output = read_stale_record_output_is_inert(
                &cx,
                &controller,
                &session.session_id,
                &workspace_path,
                &mut notification_rx,
                &stale,
            )
            .await?;
            // Restore the long-running fixture before the sibling spawn below,
            // which must stay live. The stale child has already read its own
            // script and exited, so rewriting the file cannot affect it.
            write_lody_background_script(&workspace_path, LODY_BACKGROUND_SCRIPT);
            // The child is already gone, so cleanup only settles the record and
            // releases its exec session.
            let settled = cancel_background_task(&controller, TaskOwner::Workspace, stale.id()).await;
            assert!(!settled.status.is_active(), "the second task must be settled during cleanup");

            let sibling = exercise_detached_session_registry(
                &foreign_runtime,
                &foreign_controller,
                &foreign_session.session_id,
                &background.exec_session_id,
            )
            .await;

            // Nine management requests in this flow succeed before the
            // live-output poll count is added: the two active-list reads, the
            // legacy-id output read, the sibling list, the legacy-id cancel, the
            // two post-cancel lists and the two exited-task output reads. Five
            // fail: the oversized tail, the sibling cancel and the three
            // foreign/unknown output reads.
            let management_requests = ManagementRequestTotals {
                succeeded: 9_u64.saturating_add(u64::try_from(live_output_polls).unwrap_or(u64::MAX)),
                failed: 5,
            };
            assert_management_requests_are_accounted(&inspected_agent, management_requests);

            drop(
                cx.send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert!(
                foreign_controller
                    .refresh_then_background_snapshot(TaskOwner::Workspace, &sibling)
                    .await
                    .expect("read the sibling background snapshot")
                    .entry
                    .status
                    .is_active(),
                "closing the first session must not stop a sibling's live child process"
            );
            assert_eq!(runtime_active_sessions(&first_runtime), 1);
            assert_eq!(
                first_runtime.lifecycle.lock().expect("workspace runtime lifecycle lock").state,
                crate::zed::agent::session_workspace::WorkspaceRuntimeLifecycleState::Open
            );
            let cancelled_sibling = cancel_background_task(&foreign_controller, TaskOwner::Workspace, &sibling).await;
            assert!(!cancelled_sibling.status.is_active(), "the sibling process must be stopped during test cleanup");
            drop(
                cx.send_request(CloseSessionRequest::new(foreign_session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            assert_eq!(runtime_active_sessions(&first_runtime), 0);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(30), client_connection)
        .await
        .expect("Lody management client should finish")
        .expect("Lody management protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
