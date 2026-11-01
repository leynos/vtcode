//! Cancellation and real-clock streaming physics for ACP rate-limit tests.

use super::rate_limit_test_support::{
    assert_request_floor, install_scripted_factory, provider_config, rate_limited_response, spawn_agent_connection,
    start_scripted_server, stop_agent_connection, successful_stream_response,
};
use super::tests::{PROMPT_PROVIDER_TEST_LOCK, build_wire_test_agent_with_providers};
use super::*;
use crate::acp::{InitializeRequest, NewSessionRequest};
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::routing::post;
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::{Notify, mpsc};

#[tokio::test]
async fn cancelling_during_provider_retry_wait_prevents_another_http_request() {
    let mut config = provider_config();
    let (_server, request_times) = start_scripted_server(
        &mut config,
        vec![
            rate_limited_response(&[("retry-after", "0.2")]),
            successful_stream_response(),
        ],
    )
    .await;

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard = install_scripted_factory(&config);
    let workspace = TempDir::new().expect("rate-limit cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "vtcode-rate-limit-cancel-test");
    run_cancellation_client(client_channel, workspace_path, Arc::new(Notify::new())).await;
    tokio::time::sleep(Duration::from_millis(250)).await;
    stop_agent_connection(agent_task).await;
    assert_eq!(request_times.lock().expect("cancel request times").len(), 1);
}

async fn run_cancellation_client(client_channel: Channel, workspace_path: PathBuf, retry_notice: Arc<Notify>) {
    let callback_notice = Arc::clone(&retry_notice);
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                if is_session_info_update(&notification) {
                    callback_notice.notify_one();
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
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Cancel the retry"))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = retry_notice.notified() => {}
                result = &mut prompt => panic!("prompt finished before retry notice: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(2), client_connection)
        .await
        .expect("cancelled prompt should finish")
        .expect("cancelled protocol flow should succeed");
}

fn is_session_info_update(notification: &acp::AgentNotification) -> bool {
    matches!(
        notification,
        acp::AgentNotification::SessionNotification(notification)
            if matches!(&notification.update, acp::SessionUpdate::SessionInfoUpdate(_))
    )
}

struct RunningVidaiMock {
    child: Child,
    chat_url: String,
}

impl RunningVidaiMock {
    fn start(scenario: &str) -> anyhow::Result<Self> {
        let version = Command::new("vidaimock").arg("--version").output()?;
        anyhow::ensure!(version.status.success(), "vidaimock --version failed");
        anyhow::ensure!(
            String::from_utf8_lossy(&version.stdout).contains("0.1.3"),
            "physics fixtures require VidaiMock 0.1.3"
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
                    chat_url: format!("http://127.0.0.1:{port}/v1/physics/chat/completions"),
                });
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("VidaiMock exited before accepting requests: {status}");
            }
            thread::sleep(StdDuration::from_millis(25));
        }
        drop(child.kill());
        drop(child.wait());
        anyhow::bail!("VidaiMock did not accept connections within 2.5 seconds")
    }
}

impl Drop for RunningVidaiMock {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

fn vidaimock_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock")
}

#[derive(Clone)]
struct PhysicsGatewayState {
    upstream_chat_url: String,
    attempts: Arc<AtomicUsize>,
    request_times: Arc<Mutex<Vec<StdInstant>>>,
    client: reqwest::Client,
}

async fn physics_gateway(State(state): State<PhysicsGatewayState>, body: Bytes) -> Response<Body> {
    state
        .request_times
        .lock()
        .expect("physics request times")
        .push(StdInstant::now());
    match state.attempts.fetch_add(1, Ordering::SeqCst) {
        0 => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "0.08")
            .header("x-ratelimit-limit-requests", "100")
            .header("x-ratelimit-remaining-requests", "0")
            .body(Body::from(r#"{"error":{"message":"physics quota exhausted"}}"#))
            .expect("first physics gateway response"),
        1 => Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .body(Body::from(r#"{"error":{"message":"physics quota still exhausted"}}"#))
            .expect("second physics gateway response"),
        _ => match state
            .client
            .post(&state.upstream_chat_url)
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
                    .expect("proxied VidaiMock response")
            }
            Err(error) => Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(Body::from(format!("VidaiMock proxy failure: {error}")))
                .expect("VidaiMock proxy error response"),
        },
    }
}

struct PhysicsGateway {
    address: std::net::SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

async fn start_physics_gateway(
    upstream_chat_url: String,
    request_times: Arc<Mutex<Vec<StdInstant>>>,
) -> PhysicsGateway {
    let gateway_state = PhysicsGatewayState {
        upstream_chat_url,
        attempts: Arc::new(AtomicUsize::new(0)),
        request_times,
        client: reqwest::Client::new(),
    };
    let gateway = Router::new()
        .route("/chat/completions", post(physics_gateway))
        .with_state(gateway_state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind physics gateway");
    let address = listener.local_addr().expect("physics gateway address");
    let task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve physics gateway");
    });
    PhysicsGateway { address, task }
}

struct VidaiClient {
    workspace_path: PathBuf,
    output_times: Arc<Mutex<Vec<StdInstant>>>,
    text_tx: mpsc::UnboundedSender<String>,
}

async fn run_vidaimock_client(client_channel: Channel, client: VidaiClient) {
    let VidaiClient {
        workspace_path,
        output_times: observed_output_times,
        text_tx,
    } = client;
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                if let Some(text) = streamed_token_text(notification) {
                    observed_output_times
                        .lock()
                        .expect("physics output times")
                        .push(StdInstant::now());
                    drop(text_tx.send(text));
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
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Emit four paced tokens"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(6), client_connection)
        .await
        .expect("VidaiMock ACP flow should finish")
        .expect("VidaiMock ACP flow should succeed");
}

fn streamed_token_text(notification: acp::AgentNotification) -> Option<String> {
    let acp::AgentNotification::SessionNotification(notification) = notification else {
        return None;
    };
    let acp::SessionUpdate::AgentMessageChunk(chunk) = notification.update else {
        return None;
    };
    let acp::ContentBlock::Text(text) = chunk.content else {
        return None;
    };
    (!text.text.is_empty()).then_some(text.text)
}

fn assert_vidaimock_physics(request_times: &[StdInstant], output_times: &[StdInstant], text: String) {
    assert_eq!(request_times.len(), 3);
    assert_request_floor(request_times, 0, StdDuration::from_millis(70));
    assert_request_floor(request_times, 1, StdDuration::from_millis(145));
    assert!(output_times.len() >= 4, "expected four streamed VidaiMock chunks, got {output_times:?}");
    for (index, pair) in output_times.windows(2).take(3).enumerate() {
        let cadence = pair[1].duration_since(pair[0]);
        assert!(
            cadence >= StdDuration::from_millis(200),
            "VidaiMock token interval {index} was buffered or too short: {cadence:?}"
        );
    }
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["one", "two", "three", "four"]);
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.1.3 and validates real wall-clock streaming physics"]
async fn vidaimock_gateway_preserves_retry_floors_and_stream_token_cadence() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml").expect("start VidaiMock physics fixture");
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let gateway = start_physics_gateway(vidaimock.chat_url.clone(), Arc::clone(&request_times)).await;

    let mut config = provider_config();
    config.base_url = format!("http://{}", gateway.address);
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard = install_scripted_factory(&config);
    let workspace = TempDir::new().expect("VidaiMock ACP workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let output_times = Arc::new(Mutex::new(Vec::new()));
    let (text_tx, mut text_rx) = mpsc::unbounded_channel();
    let (client_channel, agent_task) = spawn_agent_connection(&agent, "vtcode-vidaimock-rate-test");
    run_vidaimock_client(
        client_channel,
        VidaiClient {
            workspace_path,
            output_times: Arc::clone(&output_times),
            text_tx,
        },
    )
    .await;

    stop_agent_connection(agent_task).await;
    gateway.task.abort();
    drop(gateway.task.await);

    let provider_requests = request_times.lock().expect("physics request times").clone();
    let output_times = output_times.lock().expect("physics output times").clone();
    let text = std::iter::from_fn(|| text_rx.try_recv().ok()).collect::<String>();
    assert_vidaimock_physics(&provider_requests, &output_times, text);
}
