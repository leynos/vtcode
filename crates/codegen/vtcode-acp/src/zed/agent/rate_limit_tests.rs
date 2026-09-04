use super::tests::{PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers};
use super::*;
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
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
    RateLimitHeaderConfig,
};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "wire-model";
const PROVIDER: &str = "wire-test";

#[derive(Clone)]
struct ScriptedResponder {
    responses: Arc<[ResponseTemplate]>,
    request_times: Arc<Mutex<Vec<StdInstant>>>,
    next_response: Arc<AtomicUsize>,
}

impl Respond for ScriptedResponder {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        self.request_times
            .lock()
            .expect("scripted request times")
            .push(StdInstant::now());
        let response_index = self.next_response.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(response_index)
            .or_else(|| self.responses.last())
            .expect("scripted responder must have a response")
            .clone()
    }
}

struct PromptRun {
    initialized_lody: serde_json::Value,
    notifications: Vec<acp::AgentNotification>,
    request_times: Vec<StdInstant>,
    visible_text: String,
}

fn provider_config() -> CustomProviderConfig {
    CustomProviderConfig {
        name: PROVIDER.to_string(),
        display_name: "Wire rate-limit provider".to_string(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        supports_tools: Some(false),
        model: MODEL.to_string(),
        models: vec![MODEL.to_string()],
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 50,
            retry_max_backoff_ms: 50,
            retry_jitter: false,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    }
}

fn custom_provider(config: CustomProviderConfig) -> CustomProviderBackendRouter {
    let base_url = config.base_url.clone();
    CustomProviderBackendRouter::from_config(
        config,
        Some("fixture-key".to_string()),
        Some(MODEL.to_string()),
        base_url,
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    )
}

fn rate_limited_response(headers: &[(&str, &str)]) -> ResponseTemplate {
    let mut response =
        ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": {"message": "fixture quota exhausted"}}));
    for &(name, value) in headers {
        response = response.insert_header(name, value);
    }
    response
}

fn successful_stream_response() -> ResponseTemplate {
    let body = concat!(
        "data: {\"id\":\"chatcmpl-rate-limit\",\"object\":\"chat.completion.chunk\",",
        "\"created\":0,\"model\":\"wire-model\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"recovered\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-rate-limit\",\"object\":\"chat.completion.chunk\",",
        "\"created\":0,\"model\":\"wire-model\",\"choices\":[{\"index\":0,",
        "\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

async fn run_scripted_prompt(mut config: CustomProviderConfig, responses: Vec<ResponseTemplate>) -> PromptRun {
    let server = MockServer::start().await;
    config.base_url = server.uri();
    let request_times = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ScriptedResponder {
            responses: responses.into(),
            request_times: Arc::clone(&request_times),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("rate-limit wire workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-rate-limit-test"), Arc::clone(&agent))
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
            let initialized = cx
                .send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                .block_task()
                .await?;
            let initialized_lody = initialized
                .agent_capabilities
                .meta
                .as_ref()
                .and_then(|meta| meta.get("lody"))
                .cloned()
                .unwrap_or_default();
            let session = cx.send_request(NewSessionRequest::new(workspace_path)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Recover after rate limiting",
                    ))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(initialized_lody)
        });

    let initialized_lody = tokio::time::timeout(Duration::from_secs(3), client_connection)
        .await
        .expect("rate-limit protocol flow should finish")
        .expect("rate-limit protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    let notifications = std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect::<Vec<_>>();
    let visible_text = visible_text(&notifications);
    let request_times = request_times.lock().expect("scripted request times").clone();
    PromptRun {
        initialized_lody,
        notifications,
        request_times,
        visible_text,
    }
}

fn visible_text(notifications: &[acp::AgentNotification]) -> String {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::SessionNotification(notification) => match &notification.update {
                acp::SessionUpdate::AgentMessageChunk(chunk) => match &chunk.content {
                    acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn notice_values(notifications: &[acp::AgentNotification]) -> Vec<serde_json::Value> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::SessionNotification(notification)
                if matches!(&notification.update, acp::SessionUpdate::SessionInfoUpdate(_)) =>
            {
                serde_json::to_value(&notification.update).ok()
            }
            _ => None,
        })
        .filter(|update| update["_meta"]["lody"].get("notice").is_some())
        .collect()
}

fn notice_messages(notifications: &[acp::AgentNotification]) -> Vec<String> {
    notice_values(notifications)
        .into_iter()
        .filter_map(|update| update["_meta"]["lody"]["notice"]["message"].as_str().map(str::to_string))
        .collect()
}

fn rate_limit_snapshots(notifications: &[acp::AgentNotification]) -> Vec<serde_json::Value> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::ExtNotification(notification)
                if notification
                    .method
                    .as_ref()
                    .trim_start_matches('_')
                    .ends_with("lody/rate_limits/update") =>
            {
                serde_json::from_str(notification.params.get()).ok()
            }
            _ => None,
        })
        .collect()
}

fn assert_request_floor(request_times: &[StdInstant], interval_index: usize, minimum: StdDuration) {
    let actual = request_times[interval_index + 1].duration_since(request_times[interval_index]);
    assert!(actual >= minimum, "retry interval {interval_index} was {actual:?}, below floor {minimum:?}");
}

#[tokio::test]
async fn baseten_headers_publish_notices_and_lody_snapshots_across_retained_retry_floor() {
    let run = run_scripted_prompt(
        provider_config(),
        vec![
            rate_limited_response(&[
                ("retry-after", "0.08"),
                ("x-ratelimit-limit-requests", "120"),
                ("x-ratelimit-remaining-requests", "30"),
                ("x-ratelimit-limit-tokens", "12000"),
                ("x-ratelimit-remaining-tokens", "3000"),
            ]),
            rate_limited_response(&[]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.initialized_lody["rateLimits"]["version"], 1);
    assert!(run.initialized_lody["rateLimits"].get("query").is_none());
    assert_eq!(run.request_times.len(), 3);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(70));
    assert_request_floor(&run.request_times, 1, StdDuration::from_millis(145));
    assert_eq!(run.visible_text, "recovered");

    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains("provider Retry-After: 0.08"));
    assert!(messages[0].contains("request limit/min: 120"));
    assert!(messages[0].contains("requests remaining/min: 30"));
    assert!(messages[0].contains("token limit/min: 12000"));
    assert!(messages[0].contains("tokens remaining/min: 3000"));
    assert!(messages[1].contains("VTCode will retry in 0.2s"));
    assert!(
        notice_values(&run.notifications)
            .iter()
            .all(|notice| notice["_meta"]["lody"].get("rateLimits").is_none()),
        "standard ACP notices must not fabricate inline Lody rateLimits metadata"
    );

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1, "only the response with usable quota metrics should publish a snapshot");
    let limits = snapshots[0]["rateLimits"].as_array().expect("Lody rateLimits array");
    let windows = limits
        .iter()
        .flat_map(|limit| limit["windows"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().all(|window| window["usedPercent"].as_f64() == Some(75.0)));
    assert!(windows.iter().all(|window| window["windowDurationSeconds"] == 60));
    assert!(limits.iter().all(|limit| limit["scope"]["providerId"] == PROVIDER));
}

#[tokio::test]
async fn fireworks_mappings_publish_throughput_limits_but_keep_request_counters_in_the_notice() {
    let mut config = provider_config();
    config.rate_limit_headers = RateLimitHeaderConfig::for_provider_name("fireworks");
    let run = run_scripted_prompt(
        config,
        vec![
            rate_limited_response(&[
                ("retry-after", "0.05"),
                ("x-ratelimit-limit-tokens-prompt", "500"),
                ("x-ratelimit-limit-tokens-cache-adjusted-prompt", "450"),
                ("x-ratelimit-limit-tokens-generated", "125"),
                ("fireworks-prompt-tokens", "31"),
                ("fireworks-cached-prompt-tokens", "7"),
            ]),
            successful_stream_response(),
        ],
    )
    .await;

    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 1);
    for detail in [
        "prompt token limit/s: 500",
        "cache-adjusted prompt token limit/s: 450",
        "generated token limit/s: 125",
        "request prompt tokens: 31",
        "request cached prompt tokens: 7",
    ] {
        assert!(messages[0].contains(detail), "missing Fireworks notice detail: {detail}");
    }

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1);
    let limits = snapshots[0]["rateLimits"].as_array().expect("Fireworks rateLimits array");
    assert_eq!(limits.len(), 3, "per-request counters must not be duplicated as quota limits");
    assert!(
        limits
            .iter()
            .all(|limit| limit["windows"].as_array().is_some_and(Vec::is_empty))
    );
    let names = limits
        .iter()
        .filter_map(|limit| limit["limitName"].as_str())
        .collect::<Vec<_>>()
        .join(" ");
    for numeric_limit in ["500", "450", "125"] {
        assert!(names.contains(numeric_limit), "missing Fireworks numeric limit {numeric_limit}: {names}");
    }
}

#[tokio::test]
async fn together_fractional_reset_sets_retry_floor_and_one_second_lody_windows() {
    let mut config = provider_config();
    config.rate_limit_headers = RateLimitHeaderConfig::for_provider_name("together");
    let run = run_scripted_prompt(
        config,
        vec![
            rate_limited_response(&[
                ("x-ratelimit-limit", "10"),
                ("x-ratelimit-remaining", "4"),
                ("x-tokenlimit-limit", "100"),
                ("x-tokenlimit-remaining", "20"),
                ("x-ratelimit-reset", "0.075"),
            ]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.request_times.len(), 2);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(65));
    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 1);
    assert!(messages[0].contains("provider reset interval: 0.075s"));

    let snapshots = rate_limit_snapshots(&run.notifications);
    assert_eq!(snapshots.len(), 1);
    let limits = snapshots[0]["rateLimits"].as_array().expect("Together rateLimits array");
    let windows = limits
        .iter()
        .flat_map(|limit| limit["windows"].as_array().into_iter().flatten())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 2);
    assert!(windows.iter().any(|window| window["usedPercent"].as_f64() == Some(60.0)));
    assert!(windows.iter().any(|window| window["usedPercent"].as_f64() == Some(80.0)));
    assert!(windows.iter().all(|window| window["windowDurationSeconds"] == 1));
    assert_eq!(
        windows
            .iter()
            .filter(|window| window["resetsAtEpochSeconds"].is_number())
            .count(),
        1,
        "Together reset applies only to the request window"
    );
}

#[tokio::test]
async fn malformed_and_headerless_rate_limits_fall_back_to_the_capped_local_policy() {
    let run = run_scripted_prompt(
        provider_config(),
        vec![
            rate_limited_response(&[
                ("retry-after", "soon"),
                ("x-ratelimit-limit-requests", "many"),
                ("x-ratelimit-remaining-requests", "18446744073709551616"),
                ("x-ratelimit-limit-tokens", "-1"),
            ]),
            rate_limited_response(&[]),
            successful_stream_response(),
        ],
    )
    .await;

    assert_eq!(run.request_times.len(), 3);
    assert_request_floor(&run.request_times, 0, StdDuration::from_millis(40));
    assert_request_floor(&run.request_times, 1, StdDuration::from_millis(40));
    let messages = notice_messages(&run.notifications);
    assert_eq!(messages.len(), 2);
    assert!(messages[0].contains("provider Retry-After: soon"));
    assert!(messages[0].contains("VTCode will retry in 0.1s"));
    assert!(messages[1].contains("VTCode will retry in 0.1s"));
    assert!(rate_limit_snapshots(&run.notifications).is_empty());
}

#[tokio::test]
async fn cancelling_during_provider_retry_wait_prevents_another_http_request() {
    let server = MockServer::start().await;
    let mut config = provider_config();
    config.base_url = server.uri();
    let request_times = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ScriptedResponder {
            responses: vec![
                rate_limited_response(&[("retry-after", "0.2")]),
                successful_stream_response(),
            ]
            .into(),
            request_times: Arc::clone(&request_times),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("rate-limit cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let retry_notice = Arc::new(Notify::new());
    let callback_notice = Arc::clone(&retry_notice);

    let agent_connection = install_handlers(Agent.builder().name("vtcode-rate-limit-cancel-test"), Arc::clone(&agent))
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
                if matches!(
                    notification,
                    acp::AgentNotification::SessionNotification(ref notification)
                        if matches!(&notification.update, acp::SessionUpdate::SessionInfoUpdate(_))
                ) {
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
    tokio::time::sleep(Duration::from_millis(250)).await;
    agent_task.abort();
    drop(agent_task.await);
    assert_eq!(request_times.lock().expect("cancel request times").len(), 1);
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

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.1.3 and validates real wall-clock streaming physics"]
async fn vidaimock_gateway_preserves_retry_floors_and_stream_token_cadence() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml").expect("start VidaiMock physics fixture");
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let gateway_state = PhysicsGatewayState {
        upstream_chat_url: vidaimock.chat_url.clone(),
        attempts: Arc::new(AtomicUsize::new(0)),
        request_times: Arc::clone(&request_times),
        client: reqwest::Client::new(),
    };
    let gateway = Router::new()
        .route("/chat/completions", post(physics_gateway))
        .with_state(gateway_state);
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind physics gateway");
    let gateway_address = listener.local_addr().expect("physics gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve physics gateway");
    });

    let mut config = provider_config();
    config.base_url = format!("http://{gateway_address}");
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("VidaiMock ACP workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let output_times = Arc::new(Mutex::new(Vec::new()));
    let observed_output_times = Arc::clone(&output_times);
    let (text_tx, mut text_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-vidaimock-rate-test"), Arc::clone(&agent))
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
                if let acp::AgentNotification::SessionNotification(notification) = notification
                    && let acp::SessionUpdate::AgentMessageChunk(chunk) = notification.update
                    && let acp::ContentBlock::Text(text) = chunk.content
                    && !text.text.is_empty()
                {
                    observed_output_times
                        .lock()
                        .expect("physics output times")
                        .push(StdInstant::now());
                    drop(text_tx.send(text.text));
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
    agent_task.abort();
    drop(agent_task.await);
    gateway_task.abort();
    drop(gateway_task.await);

    let provider_requests = request_times.lock().expect("physics request times").clone();
    assert_eq!(provider_requests.len(), 3);
    assert_request_floor(&provider_requests, 0, StdDuration::from_millis(70));
    assert_request_floor(&provider_requests, 1, StdDuration::from_millis(145));

    let output_times = output_times.lock().expect("physics output times").clone();
    assert!(output_times.len() >= 4, "expected four streamed VidaiMock chunks, got {output_times:?}");
    for (index, pair) in output_times.windows(2).take(3).enumerate() {
        let cadence = pair[1].duration_since(pair[0]);
        assert!(
            cadence >= StdDuration::from_millis(200),
            "VidaiMock token interval {index} was buffered or too short: {cadence:?}"
        );
    }
    let text = std::iter::from_fn(|| text_rx.try_recv().ok()).collect::<String>();
    assert_eq!(text.split_whitespace().collect::<Vec<_>>(), ["one", "two", "three", "four"]);
}
