//! The ACP wire path must retain a task across repeated automatic compaction.

use super::*;
use crate::acp::{InitializeRequest, LoadSessionRequest, NewSessionRequest};
use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::routing::post;
use serde_json::Value;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use vtcode_config::core::CustomProviderApiFormat;
use vtcode_llm::providers::CustomProviderBackendRouter;

const BRANCH: &str = "fix/issue-164-preserve-complete-original-task-through-repeated-local-compaction";
const ACCEPTANCE: &str = "ISSUE164_ACCEPTANCE_KEEP_THIS_EXACT_SUFFIX_7f2c6a";
const STEERING: &str = "Resume the original task; ISSUE164_STEERING_KEEP_ALL_ACCEPTANCE_CRITERIA.";

struct RunningVidaiMock(Child);

impl Drop for RunningVidaiMock {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

impl RunningVidaiMock {
    async fn start() -> (Self, String) {
        let version = Command::new("vidaimock").arg("--version").output().expect("VidaiMock version");
        assert!(version.status.success());
        let version_text = String::from_utf8_lossy(&version.stdout);
        assert!(
            version_text.contains("0.3.1") || version_text.contains("0.4.0"),
            "unsupported VidaiMock: {version_text}"
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve VidaiMock port");
        let port = listener.local_addr().expect("VidaiMock address").port();
        drop(listener);
        let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock");
        let mut server = Self(
            Command::new("vidaimock")
                .env("VIDAIMOCK_ISOLATED", "true")
                .args(["--host", "127.0.0.1", "--port", &port.to_string()])
                .arg("--config")
                .arg(fixtures.join("scenarios/trickled-stream.toml"))
                .arg("--config-dir")
                .arg(fixtures)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start VidaiMock issue164 fixture"),
        );
        for _ in 0..100 {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                return (server, format!("http://127.0.0.1:{port}/v1/issue164/chat/completions"));
            }
            assert!(server.0.try_wait().expect("poll VidaiMock").is_none(), "VidaiMock exited at startup");
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("VidaiMock did not become ready");
    }
}

#[derive(Clone)]
struct RecordingProxy {
    upstream: String,
    client: reqwest::Client,
    requests: Arc<Mutex<Vec<Value>>>,
    normal_requests: Arc<AtomicUsize>,
}

fn is_summary(body: &Value) -> bool {
    body["messages"].as_array().is_some_and(|messages| {
        messages.len() == 1
            && messages[0]["content"]
                .as_str()
                .is_some_and(|text| text.starts_with("Summary requested at "))
    })
}

async fn forward_request(
    State(proxy): State<RecordingProxy>,
    bytes: Bytes,
) -> (StatusCode, [(header::HeaderName, &'static str); 1], String) {
    let body: Value = serde_json::from_slice(&bytes).expect("provider request JSON");
    let summary = is_summary(&body);
    let step = if summary {
        0
    } else {
        proxy.normal_requests.fetch_add(1, Ordering::SeqCst)
    };
    proxy.requests.lock().expect("record HTTP requests").push(body);
    let response = proxy
        .client
        .post(&proxy.upstream)
        .header(header::CONTENT_TYPE, "application/json")
        .header("x-issue164-step", step.to_string())
        .header("x-issue164-summary", if summary { "true" } else { "false" })
        .body(bytes)
        .send()
        .await
        .expect("forward request to local VidaiMock");
    let status = response.status();
    let text = response.text().await.expect("read VidaiMock response");
    (status, [(header::CONTENT_TYPE, "application/json")], text)
}

/// Buffered generation keeps the test focused on history, while delegating
/// every task and summary request to the real custom-provider HTTP adapter.
struct BufferedHttpProvider(CustomProviderBackendRouter);

#[async_trait]
impl LLMProvider for BufferedHttpProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        524_288
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        self.0.validate_request(request)
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.0.generate(request).await
    }
}

fn original_task() -> String {
    let mut task = format!("Work on branch `{BRANCH}`. Preserve this complete task verbatim.\n");
    for criterion in 0..110 {
        task.push_str(&format!(
            "Acceptance criterion {criterion}: retain the original requested behavior, inspect the relevant call path, and validate recovery of every requirement.\n"
        ));
    }
    task.push_str(&format!("Final acceptance criterion: {ACCEPTANCE}. Verify it after checkpoint and reload.\n"));
    task
}

async fn configured_agent(workspace: &Path, provider: &CustomProviderConfig) -> Arc<ZedAgent> {
    let mut agent = build_wire_test_agent_with_providers(workspace, std::slice::from_ref(provider)).await;
    let mut config = VTCodeConfig::default();
    config.agent.provider = "wire-test".to_string();
    config.agent.default_model = "wire-model".to_string();
    config.agent.harness.auto_compaction_enabled = true;
    config.agent.harness.auto_compaction_threshold_tokens = Some(12_000);
    config.custom_providers = vec![provider.clone()];
    agent.vt_config = Some(Box::new(config));
    agent.tool_loop_limit = 16;
    Arc::new(agent)
}

async fn run_wire_turn(
    agent: Arc<ZedAgent>,
    workspace: PathBuf,
    task: String,
    loaded_session_id: Option<acp::SessionId>,
) -> (acp::SessionId, Vec<acp::AgentNotification>) {
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("issue164-compaction"), Arc::clone(&agent))
        .connect_with(agent_channel, async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        });
    let agent_task = tokio::spawn(agent_connection);
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();
    let (session_tx, session_rx) = tokio::sync::oneshot::channel();
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
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session_id = if let Some(session_id) = loaded_session_id {
                drop(
                    cx.send_request(LoadSessionRequest::new(session_id.clone(), workspace))
                        .block_task()
                        .await?,
                );
                session_id
            } else {
                cx.send_request(NewSessionRequest::new(workspace))
                    .block_task()
                    .await?
                    .session_id
            };
            let response = cx
                .send_request(PromptRequest::new(
                    session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new(task))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            drop(session_tx.send(session_id));
            Ok(())
        });
    let result = tokio::time::timeout(Duration::from_secs(40), client_connection).await;
    agent_task.abort();
    drop(agent_task.await);
    result
        .expect("ACP compaction turn timeout")
        .expect("ACP compaction protocol flow");
    let session_id = session_rx.await.expect("ACP session identifier");
    let notifications = std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect();
    (session_id, notifications)
}

fn assert_complete_task(body: &Value, task: &str) {
    assert!(
        body["messages"]
            .as_array()
            .expect("HTTP messages")
            .iter()
            .any(|message| { message["role"] == "user" && message["content"].as_str() == Some(task) }),
        "the next HTTP request must contain the exact original user task, including its branch and final acceptance criterion"
    );
}

struct RecordingProxyServer {
    url: String,
    task: tokio::task::JoinHandle<()>,
}

impl RecordingProxyServer {
    async fn start(upstream: String, requests: Arc<Mutex<Vec<Value>>>) -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.expect("recording proxy");
        let url = format!("http://{}/v1", listener.local_addr().expect("proxy address"));
        let proxy = RecordingProxy {
            upstream,
            client: reqwest::Client::new(),
            requests,
            normal_requests: Arc::new(AtomicUsize::new(0)),
        };
        let task = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/v1/chat/completions", post(forward_request))
                    .with_state(proxy),
            )
            .await
            .expect("serve recording proxy");
        });
        Self { url, task }
    }

    async fn finish(self) {
        self.task.abort();
        // Borrow the handle so Drop still owns panic-path cleanup.
        let mut task = self;
        drop((&mut task.task).await);
    }
}

impl Drop for RecordingProxyServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct VidaiCompactionScenario {
    provider: CustomProviderConfig,
    requests: Arc<Mutex<Vec<Value>>>,
    workspace: TempDir,
    proxy: RecordingProxyServer,
    _provider_factory: PromptProviderFactoryGuard,
    _vidaimock: RunningVidaiMock,
}

impl VidaiCompactionScenario {
    async fn start() -> Self {
        let (vidaimock, upstream) = RunningVidaiMock::start().await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        let proxy = RecordingProxyServer::start(upstream, Arc::clone(&requests)).await;
        let provider = CustomProviderConfig {
            name: "wire-test".to_string(),
            display_name: "Issue164 VidaiMock".to_string(),
            base_url: proxy.url.clone(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: "wire-model".to_string(),
            models: vec!["wire-model".to_string()],
            supports_tools: Some(true),
            request_policy: CustomProviderRequestPolicyConfig { max_retries: 0, ..Default::default() },
            ..Default::default()
        };
        let provider_factory = install_http_provider_factory(&provider);
        let workspace = TempDir::new().expect("compaction workspace");
        std::fs::write(workspace.path().join("README.md"), "# Issue164 fixture\n").expect("workspace fixture");
        Self {
            provider,
            requests,
            workspace,
            proxy,
            _provider_factory: provider_factory,
            _vidaimock: vidaimock,
        }
    }
}

fn install_http_provider_factory(provider: &CustomProviderConfig) -> PromptProviderFactoryGuard {
    let factory_config = provider.clone();
    PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            let config = factory_config.clone();
            let base_url = config.base_url.clone();
            Box::new(BufferedHttpProvider(CustomProviderBackendRouter::from_config(
                config,
                Some("fixture-key".to_string()),
                Some("wire-model".to_string()),
                base_url,
                None,
                None,
                None,
                None,
                None,
                None,
            )))
        }),
    )
}

fn assert_completed_reads(notifications: &[acp::AgentNotification]) {
    let successful_reads = notifications
        .iter()
        .filter_map(|notification| {
            let acp::AgentNotification::SessionNotification(notification) = notification else {
                return None;
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return None;
            };
            (update.tool_call_id.0.starts_with("issue164-list-")
                && update.fields.status == Some(acp::ToolCallStatus::Completed))
            .then(|| update.tool_call_id.0.to_string())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(successful_reads.len(), 12, "all fixture reads must actually succeed");
}

fn assert_completed_compactions(notifications: &[acp::AgentNotification]) {
    let completed_compactions = notifications
        .iter()
        .filter(|notification| {
            let acp::AgentNotification::SessionNotification(notification) = notification else {
                return false;
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return false;
            };
            update.tool_call_id.0.starts_with("activity:compaction:")
                && update.fields.status == Some(acp::ToolCallStatus::Completed)
        })
        .count();
    assert!(
        completed_compactions >= 2,
        "must complete two production automatic compactions in the original ACP turn"
    );
}

fn assert_summary_describes_active_work(summary: &Value) {
    let text = summary["messages"][0]["content"].as_str().expect("summary input");
    assert!(text.contains("ISSUE164_WORK"), "summary input must include actual completed work");
    assert!(
        text.contains(BRANCH) || text.contains("ISSUE164_SUMMARY"),
        "summary input must describe the actual task"
    );
}

fn assert_generated_summary_installed(request: &Value) {
    assert!(
        request["messages"]
            .as_array()
            .expect("compacted HTTP messages")
            .iter()
            .any(|message| {
                message["role"] == "system"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("ISSUE164_SUMMARY"))
            }),
        "successful automatic compaction must install the generated summary before the next provider call"
    );
}

fn assert_old_continuity_budget_exceeded(request: &Value) {
    // A BPE estimate establishes actual pressure on the old 20k allowance.
    let active_tokens: usize = request["messages"]
        .as_array()
        .expect("active messages")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .map(|text| Message::assistant(text.to_string()).estimate_tokens())
        .sum();
    assert!(active_tokens > 20_000, "exercise the old continuity clipping budget: {active_tokens}");
}

fn assert_compaction_wire_requests(captured: &[Value], task: &str) {
    for request in captured.iter().filter(|body| !is_summary(body)) {
        assert_complete_task(request, task);
    }
    let summaries = captured
        .iter()
        .enumerate()
        .filter(|(_, body)| is_summary(body))
        .collect::<Vec<_>>();
    assert!(summaries.len() >= 2, "two real HTTP summary generations are required");
    for (index, summary) in &summaries {
        assert_summary_describes_active_work(summary);
        let next = captured[index + 1..]
            .iter()
            .find(|body| !is_summary(body))
            .expect("next task HTTP request");
        assert_complete_task(next, task);
        assert_generated_summary_installed(next);
    }
    let first_summary_index = summaries[0].0;
    assert_old_continuity_budget_exceeded(&captured[first_summary_index - 1]);
}

struct SavedArchive {
    path: Option<PathBuf>,
}

impl SavedArchive {
    fn from_session(agent: &ZedAgent, session_id: &acp::SessionId, task: &str) -> Self {
        let session = agent
            .session_handle(session_id)
            .expect("session lookup")
            .expect("checkpointed session");
        let path = session
            .data
            .lock()
            .expect("session data")
            .archive
            .as_ref()
            .expect("durable archive")
            .path()
            .to_path_buf();
        let archive = Self { path: Some(path.clone()) };
        let checkpoint: Value =
            serde_json::from_slice(&std::fs::read(&path).expect("checkpoint bytes")).expect("checkpoint JSON");
        assert!(
            checkpoint["messages"]
                .as_array()
                .expect("checkpoint messages")
                .iter()
                .any(|message| message["content"] == task)
        );
        archive
    }

    fn remove(mut self) {
        let path = self.path.take().expect("saved archive path");
        std::fs::remove_file(path).expect("remove test archive");
    }
}

impl Drop for SavedArchive {
    fn drop(&mut self) {
        if let Some(path) = self.path.as_ref() {
            drop(std::fs::remove_file(path));
        }
    }
}

fn assert_resumed_request(captured: &[Value], task: &str) {
    let resumed_request = captured.iter().find(|body| !is_summary(body)).expect("resumed HTTP request");
    assert_complete_task(resumed_request, task);
    assert_complete_task(resumed_request, STEERING);
}

#[tokio::test]
#[ignore = "requires VidaiMock 0.3.1 or 0.4.0; exercises real ACP, HTTP, automatic compaction and session/load"]
async fn vidaimock_preserves_complete_task_across_repeated_auto_compaction_and_reload() {
    let _provider_guard = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let scenario = VidaiCompactionScenario::start().await;
    let task = original_task();
    let agent = configured_agent(scenario.workspace.path(), &scenario.provider).await;
    let (session_id, notifications) =
        run_wire_turn(Arc::clone(&agent), scenario.workspace.path().to_path_buf(), task.clone(), None).await;

    assert_completed_reads(&notifications);
    assert_completed_compactions(&notifications);
    assert_compaction_wire_requests(&scenario.requests.lock().expect("HTTP requests").clone(), &task);
    let archive = SavedArchive::from_session(&agent, &session_id, &task);
    drop(agent);

    let fresh_agent = configured_agent(scenario.workspace.path(), &scenario.provider).await;
    assert!(fresh_agent.sessions.lock().expect("fresh sessions").is_empty());
    let captured_before_reload = scenario.requests.lock().expect("HTTP requests").len();
    drop(
        run_wire_turn(fresh_agent, scenario.workspace.path().to_path_buf(), STEERING.to_string(), Some(session_id))
            .await,
    );
    assert_resumed_request(&scenario.requests.lock().expect("HTTP requests")[captured_before_reload..], &task);
    archive.remove();
    scenario.proxy.finish().await;
}
