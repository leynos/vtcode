//! Assembled Baseten-compatible streamed usage: provider SSE to ACP usage update.
//!
//! The other usage tests inject a synthetic `LLMProvider`, so they prove the
//! projection but not the path that fills `LLMResponse::usage`. These tests
//! keep the real OpenAI-chat streaming adapter in the loop: a mock HTTP origin
//! serves the terminal-usage chunk Baseten emits, and the assertion is made on
//! the `_lody/session/usage_update` notification the ACP client sees.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use serde_json::Value;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BASETEN_MODEL: &str = "baseten/usage";
const BASETEN_PROVIDER: &str = "baseten-fixture";

/// One content delta followed by the terminal usage chunk Baseten streams when
/// `stream_options.include_usage` is set.
const TERMINAL_USAGE_STREAM: &str = concat!(
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":5,\"total_tokens\":18}}\n\n",
    "data: [DONE]\n\n",
);

/// A provider stream that never reports usage, which is what an OpenAI-
/// compatible origin does when it ignores `stream_options.include_usage`.
const USAGE_FREE_STREAM: &str = concat!(
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"finish_reason\":\"stop\"}\n\n",
    "data: [DONE]\n\n",
);

fn baseten_provider(base_url: &str) -> CustomProviderConfig {
    CustomProviderConfig {
        name: BASETEN_PROVIDER.to_string(),
        display_name: "Baseten Fixture".to_string(),
        base_url: base_url.to_string(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        supports_stream_usage: Some(true),
        model: BASETEN_MODEL.to_string(),
        models: vec![BASETEN_MODEL.to_string()],
        ..Default::default()
    }
}

fn baseten_core_config(workspace: &std::path::Path) -> CoreAgentConfig {
    CoreAgentConfig {
        model: BASETEN_MODEL.to_string(),
        api_key: "fixture-key".to_string(),
        provider: BASETEN_PROVIDER.to_string(),
        api_key_env: "BASETEN_FIXTURE_API_KEY".to_string(),
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
    }
}

/// Mounts one SSE chat-completion origin and records every request body.
async fn mount_stream(server: &MockServer, body: &'static str) -> Arc<Mutex<Vec<Value>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_mock = Arc::clone(&requests);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &Request| {
            requests_for_mock
                .lock()
                .expect("stream request mutex")
                .push(serde_json::from_slice(&request.body).expect("stream request JSON"));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
        })
        .mount(server)
        .await;
    requests
}

/// Runs one ACP prompt end to end against the mock origin and returns every
/// notification the ACP client received.
async fn run_assembled_prompt(workspace: &std::path::Path, vt_config: &VTCodeConfig) -> Vec<acp::AgentNotification> {
    let core_config = baseten_core_config(workspace);
    let startup = crate::zed::session::prepare_acp_agent_startup(&core_config, vt_config, None, true)
        .await
        .expect("prepare ACP launch with a Baseten-compatible custom provider");
    let agent = Arc::new(
        crate::zed::session::create_acp_agent(startup)
            .await
            .expect("create ACP test agent"),
    );
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("vtcode-baseten-usage-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let session_workspace = workspace.to_path_buf();
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
            let session = cx.send_request(NewSessionRequest::new(session_workspace)).block_task().await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Summarise the fixture"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("assembled Baseten prompt should finish")
        .expect("assembled Baseten protocol flow should succeed");
    agent_task.abort();
    drop(agent_task.await);

    std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect()
}

fn usage_updates(notifications: &[acp::AgentNotification]) -> Vec<Value> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::ExtNotification(notification)
                if notification.method.as_ref().trim_start_matches('_')
                    == crate::zed::agent::lody_usage::LODY_SESSION_USAGE_UPDATE_METHOD.trim_start_matches('_') =>
            {
                serde_json::from_str::<Value>(notification.params.get()).ok()
            }
            _ => None,
        })
        .collect()
}

fn baseten_vt_config(base_url: &str) -> VTCodeConfig {
    VTCodeConfig {
        custom_providers: vec![baseten_provider(base_url)],
        ..VTCodeConfig::default()
    }
}

#[tokio::test]
async fn terminal_only_baseten_usage_reaches_the_acp_client() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, TERMINAL_USAGE_STREAM).await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications = run_assembled_prompt(workspace.path(), &baseten_vt_config(&server.uri())).await;

    let updates = usage_updates(&notifications);
    assert_eq!(
        updates.len(),
        1,
        "one response with terminal-only usage must publish exactly one usage delta: {updates:?}"
    );
    let update = &updates[0];
    assert_eq!(update["usage"]["inputTokens"], 13);
    assert_eq!(update["usage"]["outputTokens"], 5);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["inputTokens"], 13);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["outputTokens"], 5);

    // The assembled path, not just the projection: the real OpenAI-chat
    // adapter opted into streamed usage for this profile.
    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
    for request in requests.iter() {
        assert_eq!(request["model"], BASETEN_MODEL);
        assert_eq!(
            request["stream_options"]["include_usage"], true,
            "an opted-in Baseten profile must request streamed usage"
        );
    }
}

#[tokio::test]
async fn baseten_stream_without_usage_publishes_no_delta() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, USAGE_FREE_STREAM).await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications = run_assembled_prompt(workspace.path(), &baseten_vt_config(&server.uri())).await;

    assert!(
        usage_updates(&notifications).is_empty(),
        "a provider stream that reports no usage must not invent a delta"
    );
    assert!(
        !requests.lock().expect("stream request mutex").is_empty(),
        "the prompt must still reach the mock origin"
    );
}
