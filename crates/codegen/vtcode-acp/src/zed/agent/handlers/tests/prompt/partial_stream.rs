//! Covers visible partial-stream failure and recovery checkpoints.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

#[tokio::test]
async fn visible_stream_failure_checkpoints_an_incomplete_turn_over_acp() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard =
        PromptProviderFactoryGuard::install("wire-test", Arc::new(|| Box::new(PartialThenFailProvider)));
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-partial-stream-test"), Arc::clone(&agent))
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
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
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
            let session = cx
                .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                .block_task()
                .await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Begin a response"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("partial-stream protocol flow should end normally");
    agent_task.abort();
    drop(agent_task.await);

    let visible_text = std::iter::from_fn(|| updates_rx.try_recv().ok())
        .filter_map(|notification| match notification.update {
            acp::SessionUpdate::AgentMessageChunk(chunk) => match chunk.content {
                acp::ContentBlock::Text(text) => Some(text.text),
                _ => None,
            },
            _ => None,
        })
        .collect::<String>();
    assert!(visible_text.contains("partial answer"));
    assert!(visible_text.contains("You can retry the prompt"));

    let session = agent
        .sessions
        .lock()
        .expect("ACP session map")
        .values()
        .next()
        .expect("wire session")
        .clone();
    let messages = session.data.lock().expect("wire session data").thread.messages();
    let incomplete = messages.last().expect("incomplete assistant message");
    assert!(incomplete.content.as_text().contains("partial answer"));
    assert!(incomplete.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete));
}
