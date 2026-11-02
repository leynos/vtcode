//! Agent-level usage attribution for custom-provider pricing and context.
//!
//! These tests drive the real [`ZedAgent::publish_lody_usage`] path with a
//! synthetic response whose `model` field is empty, so the fallback model, the
//! resolved profile, and the serialized wire notification are all exercised
//! together rather than projected in isolation.

use super::*;
use serde_json::Value;
use tokio::sync::Notify;
use vtcode_core::llm::Usage;

const WIRE_PROVIDER: &str = "wire-test";
const WIRE_MODEL: &str = "wire-model";
const OTHER_MODEL: &str = "wire-other";

/// Provider with provider-default pricing and context that a model profile can
/// override for `WIRE_MODEL`.
fn wire_provider() -> CustomProviderConfig {
    let mut profiles = BTreeMap::new();
    let _ = profiles.insert(
        WIRE_MODEL.to_string(),
        vtcode_config::core::CustomProviderProfileConfig {
            api_format: vtcode_config::core::CustomProviderApiFormat::OpenAIChat,
            context_window: Some(32_768),
            pricing: vtcode_config::core::CustomProviderPricingConfig {
                input_per_million_usd: Some(0.25),
                output_per_million_usd: Some(1.00),
                ..Default::default()
            },
            ..Default::default()
        },
    );
    CustomProviderConfig {
        name: WIRE_PROVIDER.to_string(),
        display_name: "Wire Fixture".to_string(),
        base_url: "https://wire.invalid/v1".to_string(),
        api_format: vtcode_config::core::CustomProviderApiFormat::OpenAIChat,
        context_window: Some(200_000),
        pricing: vtcode_config::core::CustomProviderPricingConfig {
            input_per_million_usd: Some(3.0),
            output_per_million_usd: Some(15.0),
            ..Default::default()
        },
        model: WIRE_MODEL.to_string(),
        models: vec![WIRE_MODEL.to_string()],
        profiles,
        ..Default::default()
    }
}

fn wire_core_config(workspace: &std::path::Path) -> CoreAgentConfig {
    CoreAgentConfig {
        model: WIRE_MODEL.to_string(),
        api_key: "fixture-key".to_string(),
        provider: WIRE_PROVIDER.to_string(),
        api_key_env: "WIRE_TEST_API_KEY".to_string(),
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

/// Publish one usage delta through a real ACP connection and return the
/// extension notifications the client received.
async fn published_usage_updates(session_model: &str, response: LLMResponse) -> Vec<acp::AgentNotification> {
    // Provider registration and the session archive directory are process
    // globals, so these tests serialize with the other prompt/provider tests.
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("usage profile workspace");
    let vt_config = VTCodeConfig {
        custom_providers: vec![wire_provider()],
        ..VTCodeConfig::default()
    };
    let startup =
        crate::zed::session::prepare_acp_agent_startup(&wire_core_config(workspace.path()), &vt_config, None, true)
            .await
            .expect("prepare ACP launch with the wire custom provider");
    let agent = Arc::new(
        crate::zed::session::create_acp_agent(startup)
            .await
            .expect("create ACP wire usage agent"),
    );

    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();
    let client_ready = Arc::new(Notify::new());
    let release_client = Arc::new(Notify::new());

    let agent_connection = install_handlers(Agent.builder().name("vtcode-usage-profile-test"), Arc::clone(&agent))
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
        .connect_with(client_channel, {
            let client_ready = Arc::clone(&client_ready);
            let release_client = Arc::clone(&release_client);
            async move |_cx: ConnectionTo<Agent>| {
                client_ready.notify_one();
                release_client.notified().await;
                Ok(())
            }
        });
    let client_task = tokio::spawn(client_connection);
    client_ready.notified().await;

    agent.publish_lody_usage(
        &acp::SessionId::new(Arc::from("session-usage-profile")),
        WIRE_PROVIDER,
        session_model,
        &response,
    );
    release_client.notify_one();

    tokio::time::timeout(Duration::from_secs(3), client_task)
        .await
        .expect("usage client connection deadline")
        .expect("usage client task")
        .expect("usage client connection");
    agent_task.abort();
    drop(agent_task.await);

    std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect()
}

fn usage_update(notifications: &[acp::AgentNotification]) -> Value {
    let updates: Vec<Value> = notifications
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
        .collect();
    assert_eq!(updates.len(), 1, "one response must publish one usage delta: {updates:?}");
    updates.into_iter().next().expect("one usage delta")
}

/// A response without a model name reports the session model, and that model's
/// profile — not the provider defaults — supplies the context window and cost.
#[tokio::test]
async fn empty_response_model_uses_session_model_profile_pricing() {
    let response = LLMResponse {
        model: String::new(),
        usage: Some(Usage {
            prompt_tokens: 13,
            completion_tokens: 5,
            ..Usage::default()
        }),
        ..LLMResponse::default()
    };

    // The duplex harness future is large enough to trip `large_futures`; pin it
    // on the heap rather than growing the test's stack frame.
    let notifications = Box::pin(published_usage_updates(WIRE_MODEL, response)).await;
    let update = usage_update(&notifications);

    assert_eq!(update["sessionId"], "session-usage-profile");
    assert_eq!(
        update["modelUsage"][WIRE_MODEL]["inputTokens"], 13,
        "the fallback session model must key the usage delta, not an empty model"
    );
    assert_eq!(
        update["usage"]["contextWindow"], 32_768,
        "the model profile must win over the provider-default context window"
    );
    let cost = update["usage"]["costUSD"].as_f64().expect("cost must be reported");
    let expected = 13.0 * 0.25e-6 + 5.0 * 1.00e-6;
    assert!(
        (cost - expected).abs() < 1e-12,
        "model-profile pricing must price the turn: expected {expected}, got {cost}"
    );
}

/// A model absent from the profile map falls back to provider defaults.
#[tokio::test]
async fn unprofiled_model_uses_provider_default_pricing() {
    let response = LLMResponse {
        model: String::new(),
        usage: Some(Usage {
            prompt_tokens: 13,
            completion_tokens: 5,
            ..Usage::default()
        }),
        ..LLMResponse::default()
    };

    let notifications = Box::pin(published_usage_updates(OTHER_MODEL, response)).await;
    let update = usage_update(&notifications);

    assert_eq!(update["modelUsage"][OTHER_MODEL]["inputTokens"], 13);
    assert_eq!(
        update["usage"]["contextWindow"], 200_000,
        "an unprofiled model must inherit the provider-default context window"
    );
    let cost = update["usage"]["costUSD"].as_f64().expect("cost must be reported");
    let expected = 13.0 * 3.0e-6 + 5.0 * 15.0e-6;
    assert!(
        (cost - expected).abs() < 1e-12,
        "provider-default pricing must price an unprofiled model: expected {expected}, got {cost}"
    );
}
