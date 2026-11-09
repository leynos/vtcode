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

/// Declared cache-read rate in USD per million tokens.
///
/// Deliberately different from the input rate so a cost that prices every
/// prompt token at the input rate cannot pass by coincidence.
const BASETEN_CACHE_READ_PER_MILLION: f64 = 0.3;

/// OpenAI Chat stream whose prompt count already includes the cached tokens.
///
/// This is the inclusive counterpart to the Anthropic fixture: `prompt_tokens`
/// is 24 uncached plus 5 cache-read tokens, and the cache detail is reported
/// separately under `prompt_tokens_details`. The ACP delta must publish that
/// total unchanged while still surfacing the cache count and pricing the two
/// token sets at their own rates.
const OPENAI_CACHE_USAGE_STREAM: &str = concat!(
    "data: {\"id\":\"chatcmpl-cache\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-cache\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"usage\":{\"prompt_tokens\":29,\"completion_tokens\":8,\"total_tokens\":37,\"prompt_tokens_details\":{\"cached_tokens\":5}}}\n\n",
    "data: [DONE]\n\n",
);

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

/// The same provider with cache metrics turned off in the global prompt-cache
/// configuration.
///
/// OpenAI-compatible providers only parse the nested cache detail when both
/// prompt caching and `surface_metrics` are enabled, so this fixture proves the
/// disabled configuration through the provider path rather than by omitting
/// the wire field from the stream.
fn baseten_core_config_without_cache_metrics(workspace: &std::path::Path) -> CoreAgentConfig {
    let mut config = baseten_core_config(workspace);
    config.prompt_cache.providers.openai.surface_metrics = false;
    config
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

/// The same OpenAI-chat provider, but with a declared cache-read rate so the
/// cost assertion can distinguish cached from uncached prompt tokens.
fn cache_priced_baseten_vt_config(base_url: &str) -> DiapasonConfig {
    let mut provider = baseten_provider(base_url);
    provider.pricing.cache_read_per_million_usd = Some(BASETEN_CACHE_READ_PER_MILLION);
    DiapasonConfig {
        custom_providers: vec![provider],
        ..DiapasonConfig::default()
    }
}

fn anthropic_vt_config(base_url: &str) -> DiapasonConfig {
    DiapasonConfig {
        custom_providers: vec![anthropic_provider(base_url)],
        ..DiapasonConfig::default()
    }
}

#[path = "assembled_usage/anthropic.rs"]
mod anthropic;

#[path = "assembled_usage/chat.rs"]
mod chat;
