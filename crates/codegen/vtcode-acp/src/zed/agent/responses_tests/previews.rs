//! Bounded, non-executable previews of streamed provider tool input.
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
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

const PREVIEW_CHAR_LIMIT: usize = 8_192;
const HIDDEN_SUFFIX: &str = "[tool-input-suffix-must-not-be-shown]";
const PREVIEW_ID: &str = "provider-input-preview:call_preview";

/// Streams an oversized custom tool call input and then holds the connection open.
async fn oversized_tool_input_responses(State(state): State<DisconnectState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let oversized = format!("{}{HIDDEN_SUFFIX}", "🚀".repeat(PREVIEW_CHAR_LIMIT + 1));
    let prefix = Bytes::from(format!(
        "{}data: {}\n\n",
        "data: \
         {\"type\":\"response.output_item.added\",\"sequence_number\":0,\"item_id\":\"ct_preview\",\
         \"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ct_preview\",\
         \"call_id\":\"call_preview\",\"name\":\"apply_patch\",\"input\":\"\",\"status\":\"in_progress\"}}\n\n",
        json!({
            "type": "response.custom_tool_call_input.delta",
            "sequence_number": 1,
            "item_id": "ct_preview",
            "call_id": "call_preview",
            "output_index": 0,
            "delta": oversized,
        })
    ));
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
        .expect("oversized tool-input Responses response")
}

type PreviewStatuses = Arc<Mutex<Vec<(String, Option<acp::ToolCallStatus>, String)>>>;

/// Proves oversized Unicode tool input is truncated to the cap without execution or suffix exposure.
#[tokio::test]
async fn oversized_unicode_tool_input_is_truncated_without_execution_or_suffix_exposure() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(oversized_tool_input_responses))
        .with_state(DisconnectState { requests: Arc::clone(&requests) });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind oversized tool-input Responses server");
    let address = listener.local_addr().expect("oversized tool-input server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve oversized tool-input Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.supports_tools = Some(true);

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("oversized preview workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let truncated_visible = Arc::new(Notify::new());
    let callback_truncated = Arc::clone(&truncated_visible);
    let preview_statuses: PreviewStatuses = Arc::new(Mutex::new(Vec::new()));
    let callback_statuses = Arc::clone(&preview_statuses);
    let model_output = Arc::new(AtomicUsize::new(0));
    let callback_model_output = Arc::clone(&model_output);

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-responses-preview-bound-test"),
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
                            if content.contains("[preview truncated]") {
                                callback_truncated.notify_one();
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
                        "Stream an oversized tool input",
                    ))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = truncated_visible.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before the preview truncation marker: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(ACP_PROTOCOL_DEADLINE, client_connection)
        .await
        .expect("oversized preview deadline")
        .expect("oversized preview protocol flow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent_task.abort();
    drop(agent_task.await);
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1, "the oversized input stream must not be replayed");
    assert_eq!(model_output.load(Ordering::SeqCst), 0, "a preview is never model output");
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
        preview_statuses.iter().all(|(id, _, _)| id == PREVIEW_ID),
        "previews must never reuse an executable tool-call id"
    );

    let bounded = preview_statuses
        .iter()
        .find(|(_, _, content)| content.contains("[preview truncated]"))
        .map(|(_, _, content)| content)
        .expect("a truncated preview update");
    let expected_prefix = "🚀".repeat(PREVIEW_CHAR_LIMIT);
    assert!(bounded.contains(&expected_prefix), "the retained prefix is character-bounded");
    assert!(!bounded.contains(HIDDEN_SUFFIX), "input beyond the preview cap must not be retained or shown");
    assert!(
        !bounded.contains(&"🚀".repeat(PREVIEW_CHAR_LIMIT + 1)),
        "the preview must not retain more than {PREVIEW_CHAR_LIMIT} characters"
    );

    assert!(
        preview_statuses
            .iter()
            .any(|(_, status, _)| *status == Some(acp::ToolCallStatus::Failed)),
        "cancelling the stream must terminate the preview"
    );
}
