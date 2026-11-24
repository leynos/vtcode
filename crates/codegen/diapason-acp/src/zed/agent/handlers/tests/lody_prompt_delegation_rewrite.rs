//! Permission-hook rewrites of an `agent` tool call driven through a prompt.
//!
//! A `PermissionRequest` hook may replace the arguments a call dispatches, so
//! these cases pin both sides of that boundary: a rewrite toward a target the
//! session does not own must fail closed, and a rewrite toward one it does own
//! must reach execution.

use super::lody_delegation_fixtures::{DelegationTestGuards, delegation_test_guards};
use super::lody_prompt_delegation_support::{
    REQUEST_TIMEOUT, assert_call_announced_before_terminal_update, assert_tool_reached_the_model,
    assert_tool_response_contains, delegation_prompt, install_agent_tool_provider_with_arguments, terminal_update_in,
    tool_notifications_through_terminal, tool_update_text,
};
use super::lody_support::{close_session, open_build_session, spawn_agent_connection, write_lody_background_fixture};
use super::*;
use diapason_core::subagents::TaskOwner;
use rstest::rstest;

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
            assert!(
                !notifications.iter().any(|notification| matches!(
                    &notification.update,
                    acp::SessionUpdate::ToolCallUpdate(update)
                        if update.tool_call_id.to_string() == "call-tool"
                            && update.fields.status == Some(acp::ToolCallStatus::InProgress)
                )),
                "a rejected rewrite must fail before the call is announced as in progress"
            );
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
                controller
                    .background_status_entries(TaskOwner::Session(session.session_id.0.as_ref()))
                    .await
                    .is_empty(),
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
            // pre-hook ownership rejection must not appear. The rejection names
            // the target the executor addressed, so asserting the rewritten id
            // pins that the hook's replacement, not the raw arguments, ran.
            let executor_target = format!("Unknown subagent id {}", owned.id);
            assert_tool_response_contains(&provider_requests, &executor_target);
            let notifications = tool_notifications_through_terminal(&mut updates_rx).await;
            assert_call_announced_before_terminal_update(&notifications, "call-tool");
            let update = terminal_update_in(&notifications);
            let text = tool_update_text(&update);
            assert!(
                text.contains(&executor_target),
                "the executor must receive the rewritten target identifier: {text}"
            );
            assert!(
                !text.contains("not owned by this session"),
                "an approved rewrite to an owned target must not be rejected for ownership: {text}"
            );
            drop(
                controller
                    .force_cancel_background(TaskOwner::Workspace, &owned.id)
                    .await
                    .expect("cancel the owned background task during cleanup")
                    .into_entry(),
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
