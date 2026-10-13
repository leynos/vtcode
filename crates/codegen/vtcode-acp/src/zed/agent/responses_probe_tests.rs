//! Issue 36 compatibility replay through the official ACP client harness.

use super::responses_tests::{
    ResponsesGatewayState, RunningVidaiMock, ScriptedResponder, provider_config, responses_gateway,
    responses_text_success, run_acp_prompts, visible_text,
};
use super::*;
use axum::Router;
use axum::routing::post;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vtcode_core::llm::provider::MessageRole as ProviderMessageRole;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn captured_response(stem: &str, status: u16) -> ResponseTemplate {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tests/fixtures/friendli/issue36")
        .join(format!("{stem}-response.sse"));
    ResponseTemplate::new(status).set_body_raw(
        std::fs::read(path).expect("read sanitized Friendli response capture"),
        if status == 200 {
            "text/event-stream"
        } else {
            "text/plain"
        },
    )
}

fn has_tool_lifecycle(notifications: &[acp::AgentNotification]) -> bool {
    notifications.iter().any(|notification| {
        matches!(
            notification,
            acp::AgentNotification::SessionNotification(notification)
                if matches!(
                    &notification.update,
                    acp::SessionUpdate::ToolCall(_) | acp::SessionUpdate::ToolCallUpdate(_)
                )
        )
    })
}

#[tokio::test]
async fn captured_custom_failures_and_no_call_prose_never_execute_and_allow_follow_up() {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![
                captured_response("custom", 500),
                captured_response("custom-named", 200),
                responses_text_success("safe follow-up"),
            ]
            .into(),
            request_times: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(3)
        .mount(&server)
        .await;
    let mut config = provider_config(server.uri());
    config.supports_tools = Some(true);
    config.request_policy.max_retries = 0;

    let run = run_acp_prompts(
        config,
        &[
            "Require one synthetic custom call",
            "Request the captured named custom call",
            "Continue safely after incompatible custom responses",
        ],
        Duration::from_secs(8),
    )
    .await;

    let requests = requests.lock().expect("probe replay requests");
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.path == "/responses"));
    assert!(
        requests
            .iter()
            .all(|request| request.body["tools"].as_array().is_some_and(|tools| !tools.is_empty()))
    );
    assert_eq!(run.stop_reasons.len(), 3);
    assert_eq!(run.active_permits, 0);
    assert!(!has_tool_lifecycle(&run.notifications), "no captured response contains an executable custom call");
    assert!(!run.messages.iter().any(|message| message.role == ProviderMessageRole::Tool));
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("No tool named `fixture_raw` is available to me."));
    assert!(visible.ends_with("safe follow-up"), "the same ACP session must accept a clean next prompt");
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates paced no-call Responses physics"]
async fn vidaimock_paced_reasoning_and_prose_with_tools_enabled_remain_non_executable() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-physics")
        .expect("start Responses VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gateway = Router::new()
        .route("/responses", post(responses_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::clone(&attempts),
            quota_failures: 0,
            request_times: Arc::new(Mutex::new(Vec::<Instant>::new())),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind issue36 Responses gateway");
    let address = listener.local_addr().expect("issue36 Responses gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve issue36 Responses gateway");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.supports_tools = Some(true);
    config.request_policy.max_retries = 0;

    let run = run_acp_prompts(
        config,
        &["Request a custom call while replaying paced no-call output"],
        Duration::from_secs(6),
    )
    .await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(paths.lock().expect("issue36 gateway paths").as_slice(), ["/responses"]);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0);
    assert!(!has_tool_lifecycle(&run.notifications));
    assert!(!run.messages.iter().any(|message| message.role == ProviderMessageRole::Tool));
    assert_eq!(
        visible_text(&run.notifications).split_whitespace().collect::<Vec<_>>(),
        ["one", "two", "three", "four"]
    );
    assert!(run.output_times.len() >= 4, "VidaiMock must emit paced prose chunks");
}
