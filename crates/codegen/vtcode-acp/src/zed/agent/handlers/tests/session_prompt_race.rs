//! Concurrent-prompt rejection over the official ACP duplex.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use std::time::Duration;
use tokio::sync::Notify;

struct BlockingPromptProvider {
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl LLMProvider for BlockingPromptProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.started.notify_one();
        self.release.notified().await;
        let response = LLMResponse::new("wire-model", "first prompt complete");
        Ok(Box::pin(futures::stream::iter([Ok(LLMStreamEvent::Completed { response: Box::new(response) })])))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("blocking prompt test must use streaming")
    }
}

/// Drive one blocked prompt and one rejected concurrent prompt over ACP.
async fn reject_concurrent_prompt(
    cx: ConnectionTo<Agent>,
    workspace_path: std::path::PathBuf,
    wait_for_provider: Arc<Notify>,
    unblock_provider: Arc<Notify>,
) -> agent_client_protocol::Result<()> {
    drop(
        cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
            .block_task()
            .await?,
    );
    let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
    let first = cx
        .send_request(PromptRequest::new(
            session.session_id.clone(),
            vec![acp::ContentBlock::Text(acp::TextContent::new("first prompt"))],
        ))
        .block_task();
    tokio::pin!(first);
    tokio::select! {
        () = wait_for_provider.notified() => {}
        response = &mut first => panic!("first prompt completed before provider blockage: {response:?}"),
    }

    let second = cx
        .send_request(PromptRequest::new(
            session.session_id,
            vec![acp::ContentBlock::Text(acp::TextContent::new("second prompt"))],
        ))
        .block_task()
        .await
        .expect_err("a concurrent prompt must be rejected");
    assert!(format!("{second:?}").contains("turn_in_progress"));

    unblock_provider.notify_one();
    let response = first.await?;
    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    Ok(())
}

#[tokio::test]
async fn concurrent_prompt_is_rejected_over_official_acp_transport() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let provider_started = Arc::clone(&started);
    let provider_release = Arc::clone(&release);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(BlockingPromptProvider {
                started: Arc::clone(&provider_started),
                release: Arc::clone(&provider_release),
            })
        }),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let inspected_agent = Arc::clone(&agent);
    let (agent_channel, client_channel) = Channel::duplex();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-prompt-race-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);
    let wait_for_provider = Arc::clone(&started);
    let unblock_provider = Arc::clone(&release);
    let client_connection = Client
        .builder()
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            reject_concurrent_prompt(cx, workspace_path, wait_for_provider, unblock_provider).await
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("prompt race protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let sessions = inspected_agent.sessions.lock().unwrap_or_else(|error| error.into_inner());
    let session = sessions.values().next().expect("wire session should remain attached");
    let messages = session.data.lock().expect("wire session data").thread.messages();
    let user_messages = messages
        .into_iter()
        .filter(|message| message.role == vtcode_core::llm::provider::MessageRole::User)
        .count();
    assert_eq!(user_messages, 1, "the rejected prompt must not mutate durable history");
}
