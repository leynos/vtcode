//! Deterministic ACP transport tests for prompt serialization and cancellation.
//!
//! These tests drive the real SACP duplex transport and the real
//! `install_handlers` wiring against a scripted provider. They deliberately do
//! not use the process-global `OnceLock` connection handle: each test owns its
//! own `Channel::duplex()` connection so overlapping prompts, steering and
//! cancellation can be ordered precisely.

use super::*;
use crate::acp::{
    CancelNotification, InitializeRequest, NewSessionRequest, PromptRequest, ProtocolVersion, SessionNotification,
    SessionUpdate, StopReason,
};
use crate::zed::agent::handlers::install_handlers;
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
use tokio::sync::{Notify, mpsc};
use vtcode_config::SubagentDiscoveryInput;
use vtcode_core::config::core::PromptCachingConfig;
use vtcode_core::config::types::{ModelSelectionSource, ReasoningEffortLevel, UiSurfacePreference};
use vtcode_core::core::agent::snapshots::{DEFAULT_CHECKPOINTS_ENABLED, DEFAULT_MAX_AGE_DAYS, DEFAULT_MAX_SNAPSHOTS};
use vtcode_core::llm::factory::{ProviderConfig, get_factory};
use vtcode_core::llm::provider::{FinishReason, LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream};

const MODEL: &str = "lifecycle-model";

/// One scripted buffered provider response.
#[derive(Clone, Copy)]
enum ScriptedStep {
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
struct ScriptedProvider {
    name: String,
    shared: Arc<ScriptedShared>,
}

impl ScriptedProvider {
    fn new(name: &str, steps: Vec<ScriptedStep>, hold_first: bool) -> Self {
        Self {
            name: name.to_string(),
            shared: Arc::new(ScriptedShared {
                steps: Mutex::new(VecDeque::from(steps)),
                call_started: Notify::new(),
                call_release: Notify::new(),
                hold_next: AtomicBool::new(hold_first),
                calls: AtomicUsize::new(0),
            }),
        }
    }

    fn call_started(&self) -> &Notify {
        &self.shared.call_started
    }

    fn release_call(&self) {
        self.shared.call_release.notify_one();
    }

    fn call_count(&self) -> usize {
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
        false
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        // Pop before waiting so a cancelled generation still consumes its
        // scripted step and cannot shift the next turn's response.
        let step = self
            .shared
            .steps
            .lock()
            .expect("scripted lifecycle steps")
            .pop_front()
            .unwrap_or(ScriptedStep::Empty);
        let _previous_calls = self.shared.calls.fetch_add(1, Ordering::SeqCst);
        self.shared.call_started.notify_one();
        if self.shared.hold_next.swap(false, Ordering::SeqCst) {
            self.shared.call_release.notified().await;
        }
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
        Err(LLMError::InvalidRequest {
            message: "lifecycle tests use buffered generation".to_string(),
            metadata: None,
        })
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

struct LifecycleHarness {
    agent: Arc<ZedAgent>,
    provider: ScriptedProvider,
    workspace: TempDir,
}

async fn lifecycle_harness(steps: Vec<ScriptedStep>, hold_first: bool) -> LifecycleHarness {
    let workspace = TempDir::new().expect("create lifecycle workspace");
    let provider_name = unique_provider_name();
    let provider = ScriptedProvider::new(&provider_name, steps, hold_first);
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

fn prompt_request(session_id: &acp::SessionId, text: &str) -> PromptRequest {
    PromptRequest::new(session_id.clone(), vec![acp::ContentBlock::Text(acp::TextContent::new(text))])
}

fn assistant_messages(notifications: &[SessionNotification]) -> Vec<String> {
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

fn error_reason(error: &acp::Error) -> Option<&str> {
    error
        .data
        .as_ref()
        .and_then(|data| data.get("reason"))
        .and_then(serde_json::Value::as_str)
}

/// Runs a client script and returns its notifications. The script receives the
/// connection, the workspace and the scripted provider.
async fn run_client<F>(harness: &LifecycleHarness, script: F) -> Vec<SessionNotification>
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
    let result = tokio::time::timeout(Duration::from_secs(15), client_connection).await;
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
async fn poll_once<F: Future + Unpin>(mut future: F) -> Poll<F::Output> {
    std::future::poll_fn(|cx| Poll::Ready(Pin::new(&mut future).poll(cx))).await
}

async fn initialize_and_create_session(
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

/// Two prompts sent while the first turn is active must both receive their own
/// response. The first turn must finish before the second starts, so no
/// assistant update may interleave.
#[tokio::test]
async fn overlapping_prompts_run_sequentially_and_both_answer() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("second-answer"),
        ],
        true,
    )
    .await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let mut first = Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task());
        tokio::select! {
            biased;
            result = &mut first => panic!("first prompt finished before the provider was released: {result:?}"),
            () = provider.call_started().notified() => {}
        }

        // One poll writes the request frame without waiting for a response, so
        // the second turn is in flight while the first still holds the gate.
        let mut second = Box::pin(cx.send_request(prompt_request(&session_id, "second prompt")).block_task());
        assert!(
            poll_once(second.as_mut()).await.is_pending(),
            "second prompt finished while the first turn was still held"
        );

        provider.release_call();
        let first_response = first.await?;
        let second_response = second.await?;
        assert_eq!(first_response.stop_reason, StopReason::EndTurn);
        assert_eq!(second_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(harness.provider.call_count(), 2, "each prompt must reach the provider once");
    assert_eq!(
        assistant_messages(&notifications),
        vec!["first-answer".to_string(), "second-answer".to_string()],
        "each turn must send its own assistant content without interleaving"
    );
}

/// A cancelled turn must not poison a prompt that was queued behind it.
#[tokio::test]
async fn cancel_then_immediate_prompt_starts_a_fresh_turn() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("second-answer"),
        ],
        true,
    )
    .await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let mut first = Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task());
        tokio::select! {
            biased;
            result = &mut first => panic!("first prompt finished before the provider was released: {result:?}"),
            () = provider.call_started().notified() => {}
        }

        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        // Frames are FIFO, so the cancel reaches the agent before the new
        // prompt. The cancel drops the held provider future, so the first
        // turn resolves without releasing it.
        let mut second = Box::pin(cx.send_request(prompt_request(&session_id, "second prompt")).block_task());
        assert!(
            poll_once(second.as_mut()).await.is_pending(),
            "second prompt finished while the first turn was cancelled"
        );

        let first_response = first.await?;
        let second_response = second.await?;
        assert_eq!(first_response.stop_reason, StopReason::Cancelled);
        assert_eq!(second_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["second-answer".to_string()],
        "the cancelled turn must stay silent and the queued steer must answer"
    );
}

/// A steer queued before `session/cancel` must survive the cancellation aimed
/// at the turn that was active when it arrived.
#[tokio::test]
async fn queued_steer_survives_cancel_aimed_at_the_active_turn() {
    let harness = lifecycle_harness(
        vec![
            ScriptedStep::Answer("first-answer"),
            ScriptedStep::Answer("steer-answer"),
        ],
        true,
    )
    .await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let mut first = Box::pin(cx.send_request(prompt_request(&session_id, "first prompt")).block_task());
        tokio::select! {
            biased;
            result = &mut first => panic!("first prompt finished before the provider was released: {result:?}"),
            () = provider.call_started().notified() => {}
        }

        // Queue the steer first, then cancel the turn it arrived during. The
        // steer frame is written before the cancel frame, so the agent admits
        // it before applying the cancellation.
        let mut steer = Box::pin(cx.send_request(prompt_request(&session_id, "steer prompt")).block_task());
        assert!(poll_once(steer.as_mut()).await.is_pending(), "steer finished while the first turn was still held");
        cx.send_notification(CancelNotification::new(session_id.clone()))?;

        let first_response = first.await?;
        let steer_response = steer.await?;
        assert_eq!(first_response.stop_reason, StopReason::Cancelled);
        assert_eq!(steer_response.stop_reason, StopReason::EndTurn);
        Ok(())
    })
    .await;

    assert_eq!(
        assistant_messages(&notifications),
        vec!["steer-answer".to_string()],
        "cancelling the active turn must not cancel or silence the queued steer"
    );
}

/// A provider that completes without content must produce an explicit error,
/// not a silent `EndTurn`.
#[tokio::test]
async fn empty_provider_output_reports_empty_response() {
    let harness = lifecycle_harness(vec![ScriptedStep::Empty], false).await;

    let notifications = run_client(&harness, async |cx, workspace, _provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let error = cx
            .send_request(prompt_request(&session_id, "empty prompt"))
            .block_task()
            .await
            .expect_err("an empty provider turn must not be acknowledged as EndTurn");
        assert_eq!(error_reason(&error), Some("empty_response"));
        Ok(())
    })
    .await;

    assert!(assistant_messages(&notifications).is_empty(), "an empty turn must not emit assistant content");
}

/// A scripted provider failure racing `session/cancel` must still answer with
/// `Cancelled`, as ACP requires. Cancellation normally wins the provider race,
/// dropping the failing future; the guarded `Failed`-after-cancel branch in the
/// turn loop covers the narrower window where the error is already in hand.
#[tokio::test]
async fn provider_failure_after_cancel_returns_cancelled() {
    let harness = lifecycle_harness(vec![ScriptedStep::Failure], true).await;

    let notifications = run_client(&harness, async |cx, workspace, provider| {
        let session_id = initialize_and_create_session(&cx, workspace).await?;
        let mut prompt = Box::pin(cx.send_request(prompt_request(&session_id, "fail prompt")).block_task());
        tokio::select! {
            biased;
            result = &mut prompt => panic!("prompt resolved before the provider was released: {result:?}"),
            () = provider.call_started().notified() => {}
        }
        cx.send_notification(CancelNotification::new(session_id.clone()))?;
        let response = prompt.await?;
        assert_eq!(response.stop_reason, StopReason::Cancelled);
        Ok(())
    })
    .await;

    assert!(
        assistant_messages(&notifications).is_empty(),
        "a cancelled failure must not send a provider failure notice"
    );
}
