//! Assembled streamed usage: provider SSE to ACP usage update.
//!
//! The other usage tests inject a synthetic `LLMProvider`, so they prove the
//! projection but not the path that fills `LLMResponse::usage`. These tests
//! keep the real provider streaming adapters in the loop: a mock HTTP origin
//! serves the usage-bearing SSE each wire format emits, and the assertion is
//! made on the `_lody/session/usage_update` notification the ACP client sees.
//!
//! Two wire formats are covered because they disagree about `input_tokens`:
//! the OpenAI chat format reports a prompt count that already contains the
//! cached tokens, while the Anthropic Messages format reports one that excludes
//! them. The assembled path must publish the same total either way.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};
use diapason_config::CustomProviderPricingConfig;
use diapason_config::core::{CustomProviderApiFormat, CustomProviderConfig};
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const BASETEN_MODEL: &str = "baseten/usage";

const BASETEN_PROVIDER: &str = "baseten-fixture";

/// Custom provider whose resolved backend is the Anthropic Messages API.
///
/// The name deliberately is not `anthropic`: the wire format, not the name,
/// decides the input-token semantics, so an exclusive-input fixture has to be
/// recognised from the backend it was routed to.
const ANTHROPIC_MODEL: &str = "anthropic-fixture/usage";

const ANTHROPIC_PROVIDER: &str = "anthropic-fixture";

/// One content delta followed by the terminal usage chunk Baseten streams when
/// `stream_options.include_usage` is set.
///
/// The usage chunk nests reasoning tokens under `completion_tokens_details`,
/// which is where OpenAI-compatible providers report them. The assembled path
/// must lift that detail into the notification rather than dropping it.
const TERMINAL_USAGE_STREAM: &str = concat!(
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":5,\"total_tokens\":18,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n",
    "data: [DONE]\n\n",
);

/// Context window the fixture profile declares, in tokens.
const BASETEN_CONTEXT_WINDOW: usize = 131_072;

/// Declared input rate in USD per million tokens.
const BASETEN_INPUT_PER_MILLION: f64 = 3.0;

/// Declared output rate in USD per million tokens.
const BASETEN_OUTPUT_PER_MILLION: f64 = 15.0;

/// Anthropic Messages stream whose prompt count excludes the cached tokens.
///
/// `message_start` carries the input side (including both cache counts) and
/// `message_delta` carries the completion count, which is where the wire
/// format splits one turn's usage. The ACP delta must add the three input
/// fields back into one total while still publishing each cache count.
const ANTHROPIC_CACHE_USAGE_STREAM: &str = concat!(
    "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_usage\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"anthropic-fixture/usage\",\"content\":[],\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":21,\"output_tokens\":0,\"cache_read_input_tokens\":5,\"cache_creation_input_tokens\":3}}}\n\n",
    "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
    "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"answer\"}}\n\n",
    "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
    "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\",\"stop_sequence\":null},\"usage\":{\"input_tokens\":21,\"output_tokens\":8}}\n\n",
    "data: {\"type\":\"message_stop\"}\n\n",
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
        context_window: Some(BASETEN_CONTEXT_WINDOW),
        pricing: CustomProviderPricingConfig {
            input_per_million_usd: Some(BASETEN_INPUT_PER_MILLION),
            output_per_million_usd: Some(BASETEN_OUTPUT_PER_MILLION),
            cache_read_per_million_usd: None,
            cache_write_per_million_usd: None,
        },
        ..Default::default()
    }
}

fn anthropic_provider(base_url: &str) -> CustomProviderConfig {
    CustomProviderConfig {
        name: ANTHROPIC_PROVIDER.to_string(),
        display_name: "Anthropic Fixture".to_string(),
        base_url: base_url.to_string(),
        api_format: CustomProviderApiFormat::AnthropicMessages,
        supports_stream_usage: Some(true),
        model: ANTHROPIC_MODEL.to_string(),
        models: vec![ANTHROPIC_MODEL.to_string()],
        context_window: Some(BASETEN_CONTEXT_WINDOW),
        ..Default::default()
    }
}

fn baseten_core_config(workspace: &std::path::Path) -> CoreAgentConfig {
    custom_provider_core_config(workspace, BASETEN_PROVIDER, BASETEN_MODEL, "BASETEN_FIXTURE_API_KEY")
}

fn anthropic_core_config(workspace: &std::path::Path) -> CoreAgentConfig {
    custom_provider_core_config(workspace, ANTHROPIC_PROVIDER, ANTHROPIC_MODEL, "ANTHROPIC_FIXTURE_API_KEY")
}

fn custom_provider_core_config(
    workspace: &std::path::Path,
    provider: &str,
    model: &str,
    api_key_env: &str,
) -> CoreAgentConfig {
    CoreAgentConfig {
        model: model.to_string(),
        api_key: "fixture-key".to_string(),
        provider: provider.to_string(),
        api_key_env: api_key_env.to_string(),
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

/// Mounts one SSE origin at `request_path` and records every request body.
///
/// The path is a parameter because the two wire formats are reached through
/// different routes: an OpenAI-chat backend posts to `/chat/completions`, while
/// the Anthropic Messages adapter appends `/messages` to the configured base
/// URL, which therefore ends in `/v1`.
async fn mount_stream(server: &MockServer, body: &'static str, request_path: &'static str) -> Arc<Mutex<Vec<Value>>> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let requests_for_mock = Arc::clone(&requests);
    Mock::given(method("POST"))
        .and(path(request_path))
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
async fn run_assembled_prompt(
    core_config: &CoreAgentConfig,
    vt_config: &DiapasonConfig,
) -> Vec<acp::AgentNotification> {
    let startup = crate::zed::session::prepare_acp_agent_startup(core_config, vt_config, None, true)
        .await
        .expect("prepare ACP launch with a custom provider");
    let agent = Arc::new(
        crate::zed::session::create_acp_agent(startup)
            .await
            .expect("create ACP test agent"),
    );
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(Agent.builder().name("diapason-assembled-usage-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);

    let session_workspace = core_config.workspace.clone();
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
        .expect("assembled prompt should finish")
        .expect("assembled protocol flow should succeed");
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

fn baseten_vt_config(base_url: &str) -> DiapasonConfig {
    DiapasonConfig {
        custom_providers: vec![baseten_provider(base_url)],
        ..DiapasonConfig::default()
    }
}

fn anthropic_vt_config(base_url: &str) -> DiapasonConfig {
    DiapasonConfig {
        custom_providers: vec![anthropic_provider(base_url)],
        ..DiapasonConfig::default()
    }
}

#[tokio::test]
async fn terminal_only_baseten_usage_reaches_the_acp_client() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, TERMINAL_USAGE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications =
        run_assembled_prompt(&baseten_core_config(workspace.path()), &baseten_vt_config(&server.uri())).await;

    let updates = usage_updates(&notifications);
    assert_eq!(
        updates.len(),
        1,
        "one response with terminal-only usage must publish exactly one usage delta: {updates:?}"
    );
    let update = &updates[0];
    assert_eq!(update["usage"]["inputTokens"], 13);
    assert_eq!(
        update["usage"]["outputTokens"], 2,
        "reasoning tokens are reported separately and excluded from the output count"
    );
    assert_eq!(update["usage"]["reasoningOutputTokens"], 3);
    assert_eq!(update["usage"]["contextWindow"], 131_072);
    // OpenAI Chat reports prompt tokens inclusive of cache tokens, and the
    // fixture declares no cache activity, so cost is the declared rates over
    // the parser's prompt and completion counts.
    let expected_cost = 13.0 * BASETEN_INPUT_PER_MILLION / 1_000_000.0 + 5.0 * BASETEN_OUTPUT_PER_MILLION / 1_000_000.0;
    let cost = update["usage"]["costUSD"]
        .as_f64()
        .expect("the declared rates must price the turn");
    assert!((cost - expected_cost).abs() < 1e-12, "expected {expected_cost} from the declared rates, got {cost}");
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["inputTokens"], 13);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["outputTokens"], 2);
    assert_eq!(update["modelUsage"][BASETEN_MODEL]["reasoningOutputTokens"], 3);

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

/// An exclusive-input backend that also reports cache activity must publish a
/// prompt total that includes the cached tokens, alongside both cache counts.
///
/// This is the end-to-end counterpart to the unit tests on
/// `normalized_turn_usage`: the OpenAI-chat fixture proves inclusive semantics,
/// this one proves exclusive semantics are added back. Asserting on both
/// `usage` and `modelUsage` keeps the two projections from drifting apart.
#[tokio::test]
async fn exclusive_anthropic_cache_usage_reaches_both_usage_fields() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, ANTHROPIC_CACHE_USAGE_STREAM, "/v1/messages").await;
    let workspace = TempDir::new().expect("Anthropic usage workspace");
    let base_url = format!("{}/v1", server.uri());
    let notifications =
        run_assembled_prompt(&anthropic_core_config(workspace.path()), &anthropic_vt_config(&base_url)).await;

    let updates = usage_updates(&notifications);
    assert_eq!(updates.len(), 1, "one response with split usage must publish exactly one usage delta: {updates:?}");
    let update = &updates[0];

    // 21 uncached + 5 cache-read + 3 cache-creation. The Anthropic wire format
    // reports `input_tokens` exclusive of the cache counts, so publishing the
    // raw value would understate the prompt by the cached tokens.
    let expected_input = 21 + 5 + 3;
    for usage in [&update["usage"], &update["modelUsage"][ANTHROPIC_MODEL]] {
        assert_eq!(usage["inputTokens"], expected_input);
        assert_eq!(usage["cacheReadInputTokens"], 5);
        assert_eq!(usage["cacheCreationInputTokens"], 3);
        assert_eq!(usage["outputTokens"], 8);
    }
    assert_eq!(update["usage"]["contextWindow"], 131_072);
    assert_eq!(
        update["modelUsage"][ANTHROPIC_MODEL], update["usage"],
        "the per-model and session projections must agree for a single model"
    );

    let requests = requests.lock().expect("stream request mutex");
    assert!(!requests.is_empty(), "the prompt must reach the mock origin");
}

#[tokio::test]
async fn baseten_stream_without_usage_publishes_no_delta() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let server = MockServer::start().await;
    let requests = mount_stream(&server, USAGE_FREE_STREAM, "/chat/completions").await;
    let workspace = TempDir::new().expect("Baseten usage workspace");
    let notifications =
        run_assembled_prompt(&baseten_core_config(workspace.path()), &baseten_vt_config(&server.uri())).await;

    assert!(
        usage_updates(&notifications).is_empty(),
        "a provider stream that reports no usage must not invent a delta"
    );
    assert!(
        !requests.lock().expect("stream request mutex").is_empty(),
        "the prompt must still reach the mock origin"
    );
}
