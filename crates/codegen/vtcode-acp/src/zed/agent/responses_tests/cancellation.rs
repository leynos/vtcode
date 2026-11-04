//! Cancellation before first output and permit release.
use super::super::provider::{StreamDeadlineTracker, StreamTimeoutPhase};
use super::super::tests::{
    PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers,
};
use super::super::*;
use super::support::*;
use crate::acp::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::routing::post;
use futures::stream;
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::{Notify, mpsc};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::llm::provider::{Message as ProviderMessage, MessageRole as ProviderMessageRole};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Cancelling during a retry wait stops further attempts, suppresses output, and releases the permit.
#[tokio::test]
async fn cancelling_responses_retry_wait_stops_requests_output_and_releases_permit() {
    let server = MockServer::start().await;
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![
                rate_limited_response(&[("retry-after", "0.3")]),
                responses_text_success("must not appear"),
            ]
            .into(),
            request_times: Arc::clone(&request_times),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let config = provider_config(server.uri());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("Responses cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let retry_notice = Arc::new(Notify::new());
    let callback_notice = Arc::clone(&retry_notice);
    let late_output = Arc::new(AtomicUsize::new(0));
    let observed_output = Arc::clone(&late_output);

    let agent_connection = install_handlers(Agent.builder().name("vtcode-responses-cancel-test"), Arc::clone(&agent))
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
                match notification {
                    acp::AgentNotification::SessionNotification(ref session)
                        if matches!(&session.update, acp::SessionUpdate::SessionInfoUpdate(_)) =>
                    {
                        callback_notice.notify_one();
                    }
                    acp::AgentNotification::SessionNotification(ref session)
                        if matches!(
                            &session.update,
                            acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_)
                        ) =>
                    {
                        let _previous_output_count = observed_output.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
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
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            drop(
                cx.send_request(SetSessionConfigOptionRequest::new(
                    session.session_id.clone(),
                    crate::zed::helpers::SESSION_CONFIG_MODEL_ID,
                    MODEL,
                ))
                .block_task()
                .await?,
            );
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Cancel Responses retry"))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = retry_notice.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before retry notice: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(ACP_PROTOCOL_DEADLINE, client_connection)
        .await
        .expect("cancelled Responses prompt deadline")
        .expect("cancelled Responses protocol flow");
    tokio::time::sleep(Duration::from_millis(350)).await;
    agent_task.abort();
    drop(agent_task.await);

    assert_responses_requests(&requests.lock().expect("Responses requests"), 1);
    assert_eq!(request_times.lock().expect("Responses request times").len(), 1);
    assert_eq!(late_output.load(Ordering::SeqCst), 0, "cancelled retry must emit no late model output");
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_acp_requests,
        0,
        "cancellation must release the provider permit"
    );
}

#[derive(Clone)]
struct PendingResponsesState {
    requests: Arc<AtomicUsize>,
    request_started: Arc<Notify>,
}

/// Serves a Responses request that records its arrival and then stays pending until aborted.
async fn pending_responses(State(state): State<PendingResponsesState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    state.request_started.notify_one();
    std::future::pending().await
}

/// Cancelling an in-flight Responses HTTP request before the first token yields no late output and frees the permit.
#[tokio::test]
async fn cancelling_pending_responses_http_before_first_token_releases_permit_without_late_output() {
    let requests = Arc::new(AtomicUsize::new(0));
    let request_started = Arc::new(Notify::new());
    let app = Router::new()
        .route("/responses", post(pending_responses))
        .with_state(PendingResponsesState {
            requests: Arc::clone(&requests),
            request_started: Arc::clone(&request_started),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind pending Responses server");
    let address = listener.local_addr().expect("pending Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve pending Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.request_policy.max_retries = 0;
    config.request_policy.first_token_timeout_seconds = 3;

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("pending Responses cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let late_output = Arc::new(AtomicUsize::new(0));
    let observed_output = Arc::clone(&late_output);

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-responses-ttft-cancel-test"),
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
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                if let acp::AgentNotification::SessionNotification(session) = notification
                    && matches!(
                        session.update,
                        acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_)
                    )
                {
                    let _previous_output_count = observed_output.fetch_add(1, Ordering::SeqCst);
                }
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
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            drop(
                cx.send_request(SetSessionConfigOptionRequest::new(
                    session.session_id.clone(),
                    crate::zed::helpers::SESSION_CONFIG_MODEL_ID,
                    MODEL,
                ))
                .block_task()
                .await?,
            );
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Cancel pending Responses HTTP"))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = request_started.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before the HTTP request became pending: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(ACP_PROTOCOL_DEADLINE, client_connection)
        .await
        .expect("pending Responses cancellation deadline")
        .expect("pending Responses cancellation protocol flow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent_task.abort();
    drop(agent_task.await);
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(late_output.load(Ordering::SeqCst), 0);
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_acp_requests,
        0,
        "cancelling an in-flight Responses HTTP request must release the provider permit"
    );
}
