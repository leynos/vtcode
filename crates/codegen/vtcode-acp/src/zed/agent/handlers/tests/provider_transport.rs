//! Covers the ACP transport path for provider rate-limit notices.
use super::super::prompt::RateLimitNoticeSink;
use super::*;
use crate::zed::types::SessionCancellation;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

/// Await the first rate-limit notice and assert its wire shape.
async fn expect_rate_limit_notice(notice_rx: &mut mpsc::UnboundedReceiver<acp::SessionNotification>) {
    let update = loop {
        let notification = tokio::time::timeout(Duration::from_secs(3), notice_rx.recv())
            .await
            .expect("the rate-limit notice should reach the client")
            .expect("the rate-limit notice channel should stay open");
        let update = serde_json::to_value(&notification.update).expect("rate-limit notice JSON");
        if update["_meta"]["lody"]["notice"].is_object() {
            break update;
        }
    };
    assert_eq!(update["sessionUpdate"], "session_info_update");
    let notice = &update["_meta"]["lody"]["notice"];
    assert_eq!(notice["level"], "warning");
    assert_eq!(notice["source"], "provider_rate_limit");
    let message = notice["message"].as_str().expect("rate-limit notice message");
    assert!(message.contains("Retry-After: 0.2"));
    assert!(message.contains("retry in"));
    assert!(
        update["_meta"]["lody"].get("rateLimits").is_none(),
        "a retry notice must not fabricate a quota window"
    );
}

/// Assert a retryable rate limit publishes a notice and recovers.
#[tokio::test]
async fn retryable_rate_limit_publishes_a_notice_over_the_acp_transport() {
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("rate-limit notice workspace");
    let agent =
        Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&VTCodeConfig::default()), None).await);
    let runtime = retry_runtime();
    let provider = RateLimitedProvider { attempts: AtomicUsize::new(0) };
    let (agent_channel, client_channel) = Channel::duplex();
    let (notice_tx, mut notice_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-rate-limit-notice-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let notice_agent = Arc::clone(&agent);
    let client_workspace = workspace.path().to_path_buf();
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(notice_tx.send(notification));
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
            let session = cx.send_request(NewSessionRequest::new(client_workspace)).block_task().await?;
            let cancellation = SessionCancellation::default();
            let notice_sink = RateLimitNoticeSink::new(notice_agent.as_ref(), &session.session_id);
            let request = RetryRequest::new(&provider, LLMRequest::default(), &runtime, &cancellation)
                .with_failure_sink(&notice_sink);
            let response = generate_with_retry(request)
                .await
                .expect("a retryable rate limit should recover");
            assert_eq!(response.content.as_deref(), Some("recovered"));

            expect_rate_limit_notice(&mut notice_rx).await;
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("the rate-limit notice flow should finish")
        .expect("the rate-limit notice flow should succeed");
    agent_task.abort();
    drop(agent_task.await);
}
