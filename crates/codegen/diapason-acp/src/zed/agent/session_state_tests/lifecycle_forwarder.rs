//! Shutdown behaviour for the ACP task lifecycle forwarder.
use super::*;
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo};
use assert_fs::TempDir;
use diapason_core::config::DiapasonConfig;

/// The initial-snapshot publisher writes into the forwarder's queue, so it
/// must stop with the forwarder.
///
/// A publisher left running outlives the receiver and keeps enqueueing into a
/// queue nothing drains, leaking a task for every attach/stop cycle. Storing
/// the handle and aborting it here is what bounds that pair.
#[tokio::test]
async fn stopping_the_forwarder_stops_its_initial_snapshot_publisher() {
    let temp = TempDir::new().unwrap();
    let agent = build_agent(temp.path()).await;
    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    let publisher = {
        let mut data = session.data.lock().unwrap();
        data.task_lifecycle_forwarder = Some(tokio::spawn(std::future::pending::<()>()));
        let publisher = tokio::spawn(std::future::pending::<()>());
        let abort_handle = publisher.abort_handle();
        data.task_lifecycle_snapshot_publisher = Some(publisher);
        abort_handle
    };

    session
        .stop_task_lifecycle_forwarder()
        .await
        .expect("stop the lifecycle forwarder");

    assert!(publisher.is_finished(), "the initial-snapshot publisher must be aborted with its forwarder");
    let data = session.data.lock().unwrap();
    assert!(data.task_lifecycle_forwarder.is_none(), "the forwarder handle must be cleared");
    assert!(
        data.task_lifecycle_snapshot_publisher.is_none(),
        "the publisher handle must be cleared once stopped"
    );
}

/// Setup that runs after teardown must not install a forwarder.
///
/// Installation spawns the forwarder and its initial-snapshot publisher
/// before it publishes their handles. A close that reached teardown in that
/// gap used to leave both tasks installed, so the session kept publishing into
/// a lifecycle that had already stopped draining it. Installation now checks
/// the closing state under the session-data lock that teardown also takes, so
/// the stop-first order aborts the install. The opposite order stays safe by
/// mutual exclusion: installation completes, and teardown then stops exactly
/// what it installed.
#[tokio::test]
async fn installation_after_teardown_does_not_restart_the_forwarder() {
    let temp = TempDir::new().unwrap();
    let mut agent = build_agent(temp.path()).await;
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.auto_restore = false;
    agent.vt_config = Some(Box::new(vt_config.clone()));
    let outcome = crate::zed::agent::optional_setup::attach_acp_subagent_controller(
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
    assert_eq!(outcome, crate::zed::agent::OptionalSetupOutcome::Attached);
    let agent = Arc::new(agent);

    // The install path needs a live client, so serve one half of a duplex and
    // wait on the explicit rendezvous the connection signals after it stores
    // the handle. A wall-clock wait here would make the race test flaky.
    let (agent_channel, client_channel) = Channel::duplex();
    let (attached_tx, attached_rx) = tokio::sync::oneshot::channel();
    let connection = crate::zed::agent::handlers::install_handlers(
        Agent.builder().name("diapason-lifecycle-race"),
        Arc::clone(&agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(&agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            attached_tx.send(()).expect("the test must observe the client attachment");
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(connection);
    attached_rx.await.expect("the agent must attach its client before setup runs");

    let session_id = agent.register_session().unwrap();
    let session = agent.session_handle(&session_id).unwrap().unwrap();

    // Stop first, exactly as a close that wins the race does.
    session.begin_close();
    session
        .stop_task_lifecycle_forwarder()
        .await
        .expect("stop the lifecycle forwarder");
    agent
        .ensure_task_lifecycle_forwarder(&session)
        .expect("a closing session must treat setup as a no-op, not a failure");

    {
        let data = session.data.lock().unwrap();
        assert!(
            data.task_lifecycle_forwarder.is_none(),
            "an install that loses to teardown must not store the forwarder handle"
        );
        assert!(
            data.task_lifecycle_snapshot_publisher.is_none(),
            "an install that loses to teardown must not store the snapshot publisher handle"
        );
    }

    drop(client_channel);
    agent_task.abort();
}
