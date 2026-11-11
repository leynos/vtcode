//! Buffered provider responses publish the same usage delta as streamed ones.
//!
//! The streamed tests prove the SSE path. These tests keep the buffered prompt
//! path in the loop: a provider that does not support streaming answers through
//! `generate`, and the assertion is made on the `_lody/session/usage_update`
//! notification the ACP client sees.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use serde_json::Value;

/// Collects the usage-update extension notifications from a client channel.
fn usage_updates(notifications: &[acp::AgentNotification]) -> Vec<Value> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::ExtNotification(notification)
                if notification.method.as_ref().trim_start_matches('_')
                    == crate::zed::agent::lody_usage::LODY_SESSION_USAGE_UPDATE_METHOD.trim_start_matches('_') =>
            {
                serde_json::from_str::<Value>(notification.params.get()).ok()
            }
            _ => None,
        })
        .collect()
}

/// Runs one ACP prompt through buffered generation and returns every
/// notification the client received.
async fn run_buffered_prompt(usage: Option<diapason_core::llm::Usage>) -> Vec<acp::AgentNotification> {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(BufferedUsageProvider { usage: usage.clone() })),
    );
    let workspace = TempDir::new().expect("buffered usage workspace");
    let session_workspace = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("diapason-buffered-usage-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                drop(notifications_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx.send_request(NewSessionRequest::new(session_workspace)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Summarise the fixture"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("buffered prompt should finish")
        .expect("buffered protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect()
}

#[tokio::test]
async fn buffered_response_usage_reaches_the_acp_client() {
    let notifications = run_buffered_prompt(Some(diapason_core::llm::Usage {
        prompt_tokens: 21,
        completion_tokens: 8,
        total_tokens: 29,
        ..Default::default()
    }))
    .await;

    let updates = usage_updates(&notifications);
    assert_eq!(
        updates.len(),
        1,
        "one buffered response with usage must publish exactly one usage delta: {updates:?}"
    );
    let update = &updates[0];
    assert_eq!(update["usage"]["inputTokens"], 21);
    assert_eq!(update["usage"]["outputTokens"], 8);
    assert_eq!(update["modelUsage"]["wire-model"]["inputTokens"], 21);
    assert_eq!(update["modelUsage"]["wire-model"]["outputTokens"], 8);
}

#[tokio::test]
async fn buffered_response_without_usage_publishes_no_delta() {
    let notifications = run_buffered_prompt(None).await;

    assert!(
        usage_updates(&notifications).is_empty(),
        "a buffered response that reports no usage must not invent a delta"
    );
}
