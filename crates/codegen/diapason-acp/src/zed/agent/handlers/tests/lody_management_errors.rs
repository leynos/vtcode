//! Capability advertisement and bounded management failures over the official ACP duplex.

use super::lody_support::lody_ext;
use super::*;
use agent_client_protocol::schema::v1::{CloseSessionRequest, InitializeRequest, NewSessionRequest};
use std::time::Duration;

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
fn advertised_capabilities_omit_lody_subagent_management_without_a_controller() {
    let capabilities = advertised_agent_capabilities(false, false);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert!(
        lody.get("subagents").is_none(),
        "an agent with no subagent controller must not advertise management: {lody}"
    );
    assert!(
        lody.get("tasks").is_none(),
        "an agent with no subagent controller must not advertise background tasks: {lody}"
    );
    assert_eq!(lody["usage"]["version"], 1, "usage reporting is independent of subagent management");
}

#[test]
fn advertised_capabilities_include_background_tasks_only_when_enabled() {
    let capabilities = advertised_agent_capabilities(true, true);
    let lody = &capabilities.meta.expect("Lody capability metadata")["lody"];

    assert_eq!(lody["tasks"]["version"], 1);
    assert_eq!(lody["tasks"]["background"], true);
}

/// Management requests fail with bounded, non-revealing reason codes.
///
/// The agent here has no subagent controller at all, which is the
/// capability-disabled configuration: management must report unavailability
/// rather than succeed, and unknown sessions or malformed parameters must not
/// distinguish themselves from each other through extra detail.
#[tokio::test]
async fn lody_management_reports_bounded_reasons_for_invalid_requests() {
    let workspace = TempDir::new().expect("Lody management workspace");
    let launch_workspace = TempDir::new().expect("separate launch workspace");
    let agent = Arc::new(build_wire_test_agent(launch_workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(
        Agent.builder().name("diapason-lody-management-errors"),
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

    let workspace_path = workspace.path().to_path_buf();
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            let initialized = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            let lody = &initialized.agent_capabilities.meta.expect("Lody capabilities")["lody"];
            assert!(
                lody.get("subagents").is_none(),
                "an agent with no subagent controller must not advertise management: {lody}"
            );

            let session = cx
                .send_request(NewSessionRequest::new(workspace_path.clone()))
                .block_task()
                .await?;

            let unknown_session = lody_ext(
                &cx,
                crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                serde_json::json!({ "sessionId": "no-such-session" }),
            )
            .await
            .expect_err("an unknown session must not resolve");
            assert_eq!(unknown_session.data, Some(serde_json::json!({ "reason": "unknown_session" })));

            let missing_session =
                lody_ext(&cx, crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD, serde_json::json!({}))
                    .await
                    .expect_err("a request without a session ID must be rejected");
            assert_eq!(
                missing_session.data.as_ref().and_then(|data| data.get("reason")),
                Some(&serde_json::json!("invalid_lody_subagent_params"))
            );

            let mistyped_tail = lody_ext(
                &cx,
                crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
                serde_json::json!({
                    "sessionId": session.session_id, "taskId": "any-task", "tail": "many",
                }),
            )
            .await
            .expect_err("a non-numeric tail must be rejected");
            assert_eq!(
                mistyped_tail.data.as_ref().and_then(|data| data.get("reason")),
                Some(&serde_json::json!("invalid_lody_subagent_params"))
            );

            let unavailable = lody_ext(
                &cx,
                crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
                serde_json::json!({ "sessionId": session.session_id }),
            )
            .await
            .expect_err("a session without a controller must report unavailability");
            assert_eq!(unavailable.data, Some(serde_json::json!({ "reason": "subagent_management_unavailable" })));

            drop(
                cx.send_request(CloseSessionRequest::new(session.session_id.clone()))
                    .block_task()
                    .await?,
            );
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(30), client_connection)
        .await
        .expect("Lody management error client should finish")
        .expect("Lody management error protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
