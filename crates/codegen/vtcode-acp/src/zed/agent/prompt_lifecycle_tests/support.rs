//! Shared harness for the prompt lifecycle transport tests: a scripted
//! provider, a temporary-workspace agent, and a duplex client runner.

use crate::acp;
use crate::acp::{
    CancelNotification, InitializeRequest, NewSessionRequest, PromptRequest, ProtocolVersion, SessionNotification,
    SessionUpdate,
};
use crate::zed::agent::handlers::install_handlers;
use crate::zed::agent::{
    AgentClientProtocolZedConfig, AuthCredentialsStoreMode, CommandsConfig, CoreAgentConfig, TimeoutsConfig,
    ToolsConfig, ZedAgent, ZedAgentSetup,
};
use crate::zed::connection::ConnectionHandle;
use crate::zed::helpers::PrimaryAgentCatalog;
use agent_client_protocol::{Agent, Channel, Client, ConnectionTo, on_receive_notification};
use assert_fs::TempDir;
use async_trait::async_trait;
use std::collections::{BTreeMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;
use tokio::sync::{Notify, mpsc};
use vtcode_config::SubagentDiscoveryInput;
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::llm::factory::{ProviderConfig, get_factory};
use vtcode_core::llm::provider::{
    FinishReason, LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, LLMStreamEvent,
};

pub(super) const MODEL: &str = "lifecycle-model";

/// One scripted buffered provider response.
#[derive(Clone, Copy)]
pub(super) enum ScriptedStep {
    Answer(&'static str),
    Empty,
    Failure,
}

/// Shared scripted-provider state, `Arc`-backed so the agent and the test can
/// observe and release each generation.
struct ScriptedShared {
    steps: Mutex<VecDeque<ScriptedStep>>,
    call_started: Notify,
    call_release: Notify,
    /// When set, the next `generate` waits for `call_release` before returning.
    hold_next: AtomicBool,
    calls: AtomicUsize,
}

#[derive(Clone)]
pub(super) struct ScriptedProvider {
    name: String,
    shared: Arc<ScriptedShared>,
    /// When set, turns call `stream` instead of `generate`.
    streaming: bool,
}

impl ScriptedProvider {
    fn new(name: &str, steps: Vec<ScriptedStep>, hold_first: bool, streaming: bool) -> Self {
        Self {
            name: name.to_string(),
            shared: Arc::new(ScriptedShared {
                steps: Mutex::new(VecDeque::from(steps)),
                call_started: Notify::new(),
                call_release: Notify::new(),
                hold_next: AtomicBool::new(hold_first),
                calls: AtomicUsize::new(0),
            }),
            streaming,
        }
    }

    fn take_step(&self) -> ScriptedStep {
        // Pop before waiting so a cancelled generation still consumes its
        // scripted step and cannot shift the next turn's response.
        self.shared
            .steps
            .lock()
            .expect("scripted lifecycle steps")
            .pop_front()
            .unwrap_or(ScriptedStep::Empty)
    }

    /// Counts a provider entry, signals the test, and applies the optional hold.
    async fn begin_call(&self, step: ScriptedStep) -> ScriptedStep {
        let _previous_calls = self.shared.calls.fetch_add(1, Ordering::SeqCst);
        self.shared.call_started.notify_one();
        if self.shared.hold_next.swap(false, Ordering::SeqCst) {
            self.shared.call_release.notified().await;
        }
        step
    }

    pub(super) fn call_started(&self) -> &Notify {
        &self.shared.call_started
    }

    pub(super) fn release_call(&self) {
        self.shared.call_release.notify_one();
    }

    pub(super) fn call_count(&self) -> usize {
        self.shared.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LLMProvider for ScriptedProvider {
    fn name(&self) -> &str {
        &self.name
    }

    fn supported_models(&self) -> Vec<String> {
        vec![MODEL.to_string()]
    }

    fn supports_streaming(&self) -> bool {
        self.streaming
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let step = self.begin_call(self.take_step()).await;
        match step {
            ScriptedStep::Answer(text) => Ok(LLMResponse {
                content: Some(text.to_string()),
                finish_reason: FinishReason::Stop,
                model: MODEL.to_string(),
                ..LLMResponse::default()
            }),
            ScriptedStep::Empty => Ok(LLMResponse {
                content: None,
                finish_reason: FinishReason::Stop,
                model: MODEL.to_string(),
                ..LLMResponse::default()
            }),
            ScriptedStep::Failure => Err(LLMError::InvalidRequest {
                message: "scripted lifecycle provider failure".to_string(),
                metadata: None,
            }),
        }
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        if !self.streaming {
            return Err(LLMError::InvalidRequest {
                message: "lifecycle buffered harness does not stream".to_string(),
                metadata: None,
            });
        }
        let step = self.begin_call(self.take_step()).await;
        match step {
            ScriptedStep::Answer(text) => {
                let response = LLMResponse {
                    content: Some(text.to_string()),
                    finish_reason: FinishReason::Stop,
                    model: MODEL.to_string(),
                    ..LLMResponse::default()
                };
                Ok(Box::pin(futures::stream::iter([Ok(LLMStreamEvent::Completed { response: Box::new(response) })])))
            }
            // An empty stream ends before the completion event, which is the
            // truncated-stream shape the retry policy must classify.
            ScriptedStep::Empty => Ok(Box::pin(futures::stream::iter(Vec::new()))),
            ScriptedStep::Failure => Err(LLMError::InvalidRequest {
                message: "scripted lifecycle provider failure".to_string(),
                metadata: None,
            }),
        }
    }
}

/// Unique provider name per test so global factory registration cannot collide
/// with another test in the same process.
fn unique_provider_name() -> String {
    static NEXT_PROVIDER_ID: AtomicUsize = AtomicUsize::new(0);
    format!("issue160-lifecycle-{}", NEXT_PROVIDER_ID.fetch_add(1, Ordering::SeqCst))
}

fn register_scripted_provider(name: &str, provider: ScriptedProvider) {
    get_factory()
        .lock()
        .expect("llm factory lock")
        .register_provider(name, move |_config: ProviderConfig| Box::new(provider.clone()));
}

async fn build_lifecycle_agent(provider_name: &str, workspace: &Path) -> ZedAgent {
    let core_config = CoreAgentConfig {
        model: MODEL.to_string(),
        api_key: "test-key".to_string(),
        provider: provider_name.to_string(),
        api_key_env: "ISSUE160_LIFECYCLE_API_KEY".to_string(),
        workspace: workspace.to_path_buf(),
        verbose: false,
        quiet: false,
        theme: "test".to_string(),
        reasoning_effort: ReasoningEffortLevel::Low,
        ui_surface: UiSurfacePreference::default(),
        prompt_cache: PromptCachingConfig::default(),
        model_source: ModelSelectionSource::WorkspaceConfig,
        custom_api_keys: BTreeMap::new(),
        checkpointing_enabled: DEFAULT_CHECKPOINTS_ENABLED,
        checkpointing_storage_dir: None,
        checkpointing_max_snapshots: DEFAULT_MAX_SNAPSHOTS,
        checkpointing_max_age_days: Some(DEFAULT_MAX_AGE_DAYS),
        max_conversation_turns: 1000,
        model_behaviour: None,
        openai_chatgpt_auth: None,
    };
    let mut discovery_input = SubagentDiscoveryInput::new(workspace.to_path_buf());
    discovery_input.include_user_agents = false;
    let discovered = vtcode_config::discover_subagents(&discovery_input).expect("discover primary agents");
    let primary_agents = PrimaryAgentCatalog::from_specs_with_default(&discovered.effective, "duck");

    let mut zed_config = AgentClientProtocolZedConfig::default();
    zed_config.allowed_workspace_roots.push(std::env::temp_dir());

    Box::pin(ZedAgent::new(ZedAgentSetup {
        config: core_config,
        credential_storage_mode: AuthCredentialsStoreMode::default(),
        zed_config,
        tools_config: ToolsConfig::default(),
        commands_config: CommandsConfig::default(),
        custom_providers: &[],
        provider_timeouts: TimeoutsConfig::default(),
        system_prompt: String::new(),
        title: Some("Issue 160 lifecycle".to_string()),
        primary_agents,
        skip_confirmations: true,
        vt_config: None,
        audit_logger: None,
    }))
    .await
    .expect("build lifecycle ACP agent")
}

pub(super) struct LifecycleHarness {
    pub(super) agent: Arc<ZedAgent>,
    pub(super) provider: ScriptedProvider,
    pub(super) workspace: TempDir,
}

pub(super) async fn lifecycle_harness(steps: Vec<ScriptedStep>, hold_first: bool) -> LifecycleHarness {
    lifecycle_harness_with_mode(steps, hold_first, false).await
}

/// Harness whose provider streams. Pair it with a paused clock so retry
/// back-off costs virtual time instead of wall-clock time.
pub(super) async fn streaming_lifecycle_harness(steps: Vec<ScriptedStep>, hold_first: bool) -> LifecycleHarness {
    lifecycle_harness_with_mode(steps, hold_first, true).await
}

async fn lifecycle_harness_with_mode(steps: Vec<ScriptedStep>, hold_first: bool, streaming: bool) -> LifecycleHarness {
    let workspace = TempDir::new().expect("create lifecycle workspace");
    let provider_name = unique_provider_name();
    let provider = ScriptedProvider::new(&provider_name, steps, hold_first, streaming);
    let agent = Arc::new(build_lifecycle_agent(&provider_name, workspace.path()).await);
    register_scripted_provider(&provider_name, provider.clone());
    LifecycleHarness { agent, provider, workspace }
}

fn spawn_agent_connection(
    agent: Arc<ZedAgent>,
) -> (tokio::task::JoinHandle<agent_client_protocol::Result<()>>, Channel) {
    let (agent_channel, client_channel) = Channel::duplex();
    let connection = install_handlers(Agent.builder().name("issue160-lifecycle"), agent.clone()).connect_with(
        agent_channel,
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        },
    );
    (tokio::spawn(connection), client_channel)
}

pub(super) fn prompt_request(session_id: &acp::SessionId, text: &str) -> PromptRequest {
    PromptRequest::new(session_id.clone(), vec![acp::ContentBlock::Text(acp::TextContent::new(text))])
}

pub(super) fn assistant_messages(notifications: &[SessionNotification]) -> Vec<String> {
    notifications
        .iter()
        .filter_map(|notification| {
            let SessionUpdate::AgentMessageChunk(chunk) = &notification.update else {
                return None;
            };
            match &chunk.content {
                acp::ContentBlock::Text(text) => Some(text.text.clone()),
                _ => None,
            }
        })
        .collect()
}

pub(super) fn error_reason(error: &acp::Error) -> Option<&str> {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str)
}

/// Runs a client script and returns its notifications. The script receives the
/// connection, the workspace and the scripted provider.
pub(super) async fn run_client<F>(harness: &LifecycleHarness, script: F) -> Vec<SessionNotification>
where
    F: AsyncFnOnce(ConnectionTo<Agent>, PathBuf, ScriptedProvider) -> agent_client_protocol::Result<()>,
{
    run_client_with_timeout(harness, Duration::from_secs(15), script).await
}

/// Runs a client script under an explicit connection timeout. Streaming retry
/// tests need a budget larger than the provider's virtual back-off sum.
pub(super) async fn run_client_with_timeout<F>(
    harness: &LifecycleHarness,
    client_timeout: Duration,
    script: F,
) -> Vec<SessionNotification>
where
    F: AsyncFnOnce(ConnectionTo<Agent>, PathBuf, ScriptedProvider) -> agent_client_protocol::Result<()>,
{
    let (agent_task, client_channel) = spawn_agent_connection(Arc::clone(&harness.agent));
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: SessionNotification, _cx| {
                drop(notifications_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, {
            let workspace = harness.workspace.path().to_path_buf();
            let provider = harness.provider.clone();
            async move |cx: ConnectionTo<Agent>| script(cx, workspace, provider).await
        });
    let result = tokio::time::timeout(client_timeout, client_connection).await;
    agent_task.abort();
    drop(agent_task.await);
    result
        .expect("lifecycle client connection timeout")
        .expect("lifecycle protocol flow");
    std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect()
}

/// Polls a request future exactly once. The first poll writes the request onto
/// the duplex channel, which is enough to order it against a later frame
/// without relying on a timing delay.
pub(super) async fn poll_once<F: Future + Unpin>(mut future: F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut future).poll(cx))).await
}

/// Waits until `expected` prompts are acquiring the session's prompt gate.
///
/// A steer only becomes observable here after the agent admits the request and
/// its spawned task reaches the gate, so this proves the steer is queued before
/// a test releases or cancels the turn it waits for. Yielding, rather than
/// sleeping, keeps the ordering deterministic.
pub(super) async fn wait_for_gate_waiters(agent: &ZedAgent, session_id: &acp::SessionId, expected: usize) {
    for _ in 0..10_000 {
        let waiters = agent
            .session_handle(session_id)
            .expect("look up lifecycle session")
            .expect("lifecycle session exists")
            .prompt_gate_waiter_count();
        if waiters >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the prompt gate never reported {expected} waiter(s)");
}

pub(super) async fn initialize_and_create_session(
    cx: &ConnectionTo<Agent>,
    workspace: PathBuf,
) -> agent_client_protocol::Result<acp::SessionId> {
    drop(
        cx.send_request(InitializeRequest::new(ProtocolVersion::V1))
            .block_task()
            .await?,
    );
    let session = cx.send_request(NewSessionRequest::new(workspace)).block_task().await?;
    Ok(session.session_id)
}

/// Sends the first prompt and waits until the scripted provider has been
/// entered while holding its release, proving the turn owns the session gate.
/// Returns the still-pending prompt response.
pub(super) async fn hold_first_prompt_until_provider_starts<F>(provider: &ScriptedProvider, first: F) -> F
where
    F: Future + Unpin,
    F::Output: std::fmt::Debug,
{
    let mut first = first;
    tokio::select! {
        biased;
        result = &mut first => panic!("first prompt finished before the provider was released: {result:?}"),
        () = provider.call_started().notified() => {}
    }
    first
}
