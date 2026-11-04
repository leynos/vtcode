//! Cancellation while provider tool input is still streaming.
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

/// Streams a partial custom tool call input and then keeps the connection open.
async fn pending_tool_input_responses(State(state): State<DisconnectState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let prefix = Bytes::from_static(
        concat!(
            "data: {\"type\":\"response.output_item.added\",\"sequence_number\":0,\"item_id\":\"ct_cancel\",\"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ct_cancel\",\"call_id\":\"call_cancel\",\"name\":\"apply_patch\",\"input\":\"\",\"status\":\"in_progress\"}}\n\n",
            "data: {\"type\":\"response.custom_tool_call_input.delta\",\"sequence_number\":1,\"item_id\":\"ct_cancel\",\"call_id\":\"call_cancel\",\"output_index\":0,\"delta\":\"*** Begin Patch\\n*** Delete File: cancel-must-not-run\"}\n\n",
        )
        .as_bytes(),
    );
    let body = Body::from_stream(stream::unfold((0_u8, prefix), |(step, prefix)| async move {
        match step {
            0 => Some((Ok::<Bytes, std::io::Error>(prefix.clone()), (1, prefix))),
            _ => std::future::pending().await,
        }
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("pending tool-input Responses response")
}
/// Proves cancelling partial tool input fails the preview without execution, retry, or late model output.
#[tokio::test]
async fn cancelling_partial_tool_input_fails_preview_without_execution_retry_or_late_model_output() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(pending_tool_input_responses))
        .with_state(DisconnectState { requests: Arc::clone(&requests) });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind pending tool-input Responses server");
    let address = listener.local_addr().expect("pending tool-input Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve pending tool-input Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.supports_tools = Some(true);

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("tool-input cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let preview_visible = Arc::new(Notify::new());
    let callback_preview_visible = Arc::clone(&preview_visible);
    let preview_statuses = Arc::new(Mutex::new(Vec::new()));
    let callback_statuses = Arc::clone(&preview_statuses);
    let late_model_output = Arc::new(AtomicUsize::new(0));
    let callback_model_output = Arc::clone(&late_model_output);

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-responses-tool-cancel-test"),
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
                if let acp::AgentNotification::SessionNotification(session) = notification {
                    match session.update {
                        acp::SessionUpdate::ToolCall(call) => {
                            callback_statuses.lock().expect("preview statuses").push((
                                call.tool_call_id.0.as_ref().to_string(),
                                Some(call.status),
                                tool_content_text(&call.content),
                            ));
                        }
                        acp::SessionUpdate::ToolCallUpdate(update) => {
                            let content = update.fields.content.as_deref().map(tool_content_text).unwrap_or_default();
                            if content.contains("cancel-must-not-run") {
                                callback_preview_visible.notify_one();
                            }
                            callback_statuses.lock().expect("preview statuses").push((
                                update.tool_call_id.0.as_ref().to_string(),
                                update.fields.status,
                                content,
                            ));
                        }
                        acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_) => {
                            let _previous_output_count = callback_model_output.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => {}
                    }
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
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Cancel partial tool input",
                    ))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = preview_visible.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before tool preview: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(ACP_PROTOCOL_DEADLINE, client_connection)
        .await
        .expect("tool-input cancellation deadline")
        .expect("tool-input cancellation protocol flow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent_task.abort();
    drop(agent_task.await);
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(late_model_output.load(Ordering::SeqCst), 0);
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_acp_requests,
        0
    );
    let preview_statuses = preview_statuses.lock().expect("preview statuses");
    assert!(!preview_statuses.is_empty());
    assert!(
        preview_statuses
            .iter()
            .all(|(id, _, _)| id == "provider-input-preview:call_cancel")
    );
    assert!(preview_statuses.iter().any(|(_, status, content)| {
        *status == Some(acp::ToolCallStatus::Pending) && content.contains("cancel-must-not-run")
    }));
    assert!(
        preview_statuses
            .iter()
            .any(|(_, status, _)| *status == Some(acp::ToolCallStatus::Failed))
    );
}
