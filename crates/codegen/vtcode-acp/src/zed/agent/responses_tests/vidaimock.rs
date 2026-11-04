//! End-to-end coverage against the VidaiMock Responses gateway fixture.
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

struct RunningVidaiMock {
    child: Child,
    responses_url: String,
}

impl RunningVidaiMock {
    /// Starts VidaiMock on a free port for the given scenario and waits until it accepts connections.
    fn start(scenario: &str, provider_path: &str) -> anyhow::Result<Self> {
        let version = Command::new("vidaimock").arg("--version").output()?;
        anyhow::ensure!(version.status.success(), "vidaimock --version failed");
        anyhow::ensure!(
            String::from_utf8_lossy(&version.stdout).contains("0.3.1"),
            "Responses physics fixtures require VidaiMock 0.3.1"
        );
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let fixtures = vidaimock_fixture_root();
        let mut child = Command::new("vidaimock")
            .env("VIDAIMOCK_ISOLATED", "true")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(fixtures.join("scenarios").join(scenario))
            .arg("--config-dir")
            .arg(fixtures)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let address = ("127.0.0.1", port);
        for _ in 0..100 {
            if TcpStream::connect(address).is_ok() {
                return Ok(Self {
                    child,
                    responses_url: format!("http://127.0.0.1:{port}/v1/{provider_path}/responses"),
                });
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("VidaiMock exited before accepting Responses requests: {status}");
            }
            thread::sleep(StdDuration::from_millis(25));
        }
        drop(child.kill());
        drop(child.wait());
        anyhow::bail!("VidaiMock did not accept Responses requests within 2.5 seconds")
    }
}

impl Drop for RunningVidaiMock {
    /// Terminate and reap the VidaiMock child process.
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

/// Returns the crate-relative path to the VidaiMock scenario fixtures.
fn vidaimock_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock")
}

#[derive(Clone)]
struct ResponsesGatewayState {
    upstream_responses_url: String,
    paths: Arc<Mutex<Vec<String>>>,
    attempts: Arc<AtomicUsize>,
    quota_failures: usize,
    request_times: Arc<Mutex<Vec<StdInstant>>>,
    client: reqwest::Client,
}

/// Proxies Responses requests to VidaiMock, injecting scripted quota failures first.
async fn responses_gateway(State(state): State<ResponsesGatewayState>, body: Bytes) -> Response<Body> {
    state
        .paths
        .lock()
        .expect("Responses gateway paths")
        .push("/responses".to_string());
    state
        .request_times
        .lock()
        .expect("Responses gateway request times")
        .push(StdInstant::now());
    if state.attempts.fetch_add(1, Ordering::SeqCst) < state.quota_failures {
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "0.08")
            .header("x-ratelimit-limit-requests", "100")
            .header("x-ratelimit-remaining-requests", "0")
            .body(Body::from(r#"{"error":{"message":"Responses physics quota exhausted"}}"#))
            .expect("Responses quota gateway response");
    }
    match state
        .client
        .post(&state.upstream_responses_url)
        .header(header::CONTENT_TYPE.as_str(), "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(upstream) => {
            let status = upstream.status();
            let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
            let mut response = Response::builder().status(status);
            if let Some(content_type) = content_type {
                response = response.header(header::CONTENT_TYPE, content_type);
            }
            response
                .body(Body::from_stream(upstream.bytes_stream()))
                .expect("proxied Responses body")
        }
        Err(error) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("VidaiMock Responses proxy failure: {error}")))
            .expect("VidaiMock Responses proxy error"),
    }
}

/// Proves a real VidaiMock Responses stream crosses ACP with paced chunks, quota headers, and usage.
#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates real Responses streaming physics"]
async fn vidaimock_custom_responses_crosses_acp_with_paced_output_and_usage() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-physics")
        .expect("start Responses VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let gateway = Router::new()
        .route("/responses", post(responses_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::new(AtomicUsize::new(0)),
            quota_failures: 1,
            request_times: Arc::clone(&request_times),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind Responses gateway");
    let address = listener.local_addr().expect("Responses gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve Responses gateway");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Emit the Responses physics fixture"],
        ACP_PROTOCOL_DEADLINE,
    )
    .await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(paths.lock().expect("Responses gateway paths").as_slice(), ["/responses", "/responses"]);
    let request_times = request_times.lock().expect("Responses gateway request times");
    assert_eq!(request_times.len(), 2);
    assert!(request_times[1].duration_since(request_times[0]) >= StdDuration::from_millis(75));
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_acp_requests, 0);
    assert_eq!(
        visible_text(&run.notifications).split_whitespace().collect::<Vec<_>>(),
        ["one", "two", "three", "four"]
    );
    assert_eq!(visible_reasoning(&run.notifications), "fixture reasoning");
    let rate_limits = ext_notifications(&run.notifications, "lody/rate_limits/update");
    assert_eq!(rate_limits.len(), 1, "the Responses gateway 429 headers must reach ACP");
    assert_eq!(rate_limits[0]["rateLimits"][0]["windows"][0]["usedPercent"], 100.0);
    assert!(run.output_times.len() >= 4, "expected four paced Responses output chunks");
    for pair in run.output_times.windows(2).take(3) {
        assert!(
            pair[1].duration_since(pair[0]) >= StdDuration::from_millis(200),
            "VidaiMock Responses scenario did not preserve the configured 250 ms cadence"
        );
    }
    let usage = ext_notifications(&run.notifications, "lody/session/usage_update");
    assert_eq!(usage.len(), 1, "Responses terminal usage must reach ACP");
    assert_eq!(usage[0]["usage"]["inputTokens"], 13);
    assert_eq!(usage[0]["usage"]["outputTokens"], 5);
    assert_eq!(usage[0]["usage"]["reasoningOutputTokens"], 2);
}

/// Proves a real truncated VidaiMock Responses stream is visible, marked incomplete, and not retried.
#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates a real truncated Responses stream"]
async fn vidaimock_responses_truncated_stream_is_visible_incomplete_and_not_retried() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-physics-truncated")
        .expect("start truncating Responses VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let gateway = Router::new()
        .route("/responses", post(responses_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::new(AtomicUsize::new(0)),
            quota_failures: 0,
            request_times: Arc::new(Mutex::new(Vec::new())),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind truncating Responses gateway");
    let address = listener.local_addr().expect("truncating Responses gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway)
            .await
            .expect("serve truncating Responses gateway");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Emit output before the Responses connection is cut"],
        ACP_PROTOCOL_DEADLINE,
    )
    .await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(paths.lock().expect("Responses gateway paths").as_slice(), ["/responses"]);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_acp_requests, 0);
    assert!(!visible_text(&run.notifications).trim().is_empty());
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && !message.content.as_text().trim().is_empty()
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}
