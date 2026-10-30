//! ACP protocol setup, prompt driving and observation collection for Responses regression tests.

use super::provider_failure_capture_tests::ProviderFailureCaptureGuard;
use super::*;

pub(in super::super) struct AcpRun {
    pub(in super::super) provider_failures: Vec<String>,
    pub(in super::super) notifications: Vec<acp::AgentNotification>,
    pub(in super::super) output_times: Vec<StdInstant>,
    pub(in super::super) stop_reasons: Vec<acp::StopReason>,
    pub(in super::super) messages: Vec<ProviderMessage>,
    pub(in super::super) checkpoint_json: Option<String>,
    pub(in super::super) active_acp_requests: usize,
}
pub(in super::super) async fn run_acp_prompts(
    config: CustomProviderConfig,
    prompts: &[&str],
    timeout: Duration,
) -> AcpRun {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let provider_failures = Arc::new(Mutex::new(Vec::new()));
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("Responses ACP workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let _failure_guard = ProviderFailureCaptureGuard::install(
        agent.provider_runtime.for_provider(PROVIDER),
        Arc::clone(&provider_failures),
    );
    let (notifications_tx, notifications_rx) = mpsc::unbounded_channel();
    let output_times = Arc::new(Mutex::new(Vec::new()));
    let observations = PromptObservations {
        notifications_tx,
        output_times: Arc::clone(&output_times),
    };
    let scenario = PromptScenario {
        workspace_path,
        prompt_texts: prompts.iter().map(|prompt| (*prompt).to_string()).collect(),
        timeout,
    };
    let completion = drive_acp_prompts(Arc::clone(&agent), scenario, observations).await;
    let capture = AcpRunCapture { notifications_rx, output_times, provider_failures };
    capture.collect(&agent, completion)
}

struct PromptObservations {
    notifications_tx: mpsc::UnboundedSender<acp::AgentNotification>,
    output_times: Arc<Mutex<Vec<StdInstant>>>,
}

impl PromptObservations {
    fn record(&self, notification: acp::AgentNotification) {
        match &notification {
            acp::AgentNotification::SessionNotification(acp::SessionNotification {
                update:
                    acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
                        content: acp::ContentBlock::Text(acp::TextContent { text, .. }),
                        ..
                    }),
                ..
            }) if !text.is_empty() => {
                self.output_times
                    .lock()
                    .expect("Responses output times")
                    .push(StdInstant::now());
            }
            _ => {}
        }
        drop(self.notifications_tx.send(notification));
    }
}

async fn drive_acp_prompts(
    agent: Arc<ZedAgent>,
    scenario: PromptScenario,
    observations: PromptObservations,
) -> PromptCompletion {
    let PromptScenario { workspace_path, prompt_texts, timeout } = scenario;
    let (agent_channel, client_channel) = Channel::duplex();
    let agent_connection = install_handlers(Agent.builder().name("vtcode-responses-test"), Arc::clone(&agent))
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
                observations.record(notification);
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drive_prompt_requests(cx, workspace_path, prompt_texts).await
        });

    let completion = tokio::time::timeout(timeout, client_connection)
        .await
        .expect("Responses ACP protocol deadline")
        .expect("Responses ACP protocol flow");
    agent_task.abort();
    drop(agent_task.await);

    completion
}

async fn drive_prompt_requests(
    cx: ConnectionTo<Agent>,
    workspace_path: PathBuf,
    prompt_texts: Vec<String>,
) -> agent_client_protocol::Result<PromptCompletion> {
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
    let mut stop_reasons = Vec::with_capacity(prompt_texts.len());
    for prompt in prompt_texts {
        let response = cx
            .send_request(PromptRequest::new(
                session.session_id.clone(),
                vec![acp::ContentBlock::Text(acp::TextContent::new(prompt))],
            ))
            .block_task()
            .await?;
        stop_reasons.push(response.stop_reason);
    }
    Ok(PromptCompletion { session_id: session.session_id, stop_reasons })
}

struct PromptScenario {
    workspace_path: PathBuf,
    prompt_texts: Vec<String>,
    timeout: Duration,
}

struct PromptCompletion {
    session_id: acp::SessionId,
    stop_reasons: Vec<acp::StopReason>,
}

struct AcpRunCapture {
    notifications_rx: mpsc::UnboundedReceiver<acp::AgentNotification>,
    output_times: Arc<Mutex<Vec<StdInstant>>>,
    provider_failures: Arc<Mutex<Vec<String>>>,
}

impl AcpRunCapture {
    fn collect(mut self, agent: &ZedAgent, completion: PromptCompletion) -> AcpRun {
        let PromptCompletion { session_id, stop_reasons } = completion;
        let session = agent
            .session_handle(&session_id)
            .expect("Responses session lookup")
            .expect("Responses ACP session");
        let (messages, checkpoint_path) = {
            let data = session.data.lock().expect("Responses session data");
            (data.thread.messages(), data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
        };
        let checkpoint_json = checkpoint_path.and_then(|path| std::fs::read_to_string(path).ok());
        let active_acp_requests = agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_acp_requests;
        let notifications = std::iter::from_fn(|| self.notifications_rx.try_recv().ok()).collect();
        let output_times = self.output_times.lock().expect("Responses output times").clone();
        let provider_failures = self.provider_failures.lock().expect("captured provider failures").clone();

        AcpRun {
            provider_failures,
            notifications,
            output_times,
            stop_reasons,
            messages,
            checkpoint_json,
            active_acp_requests,
        }
    }
}
