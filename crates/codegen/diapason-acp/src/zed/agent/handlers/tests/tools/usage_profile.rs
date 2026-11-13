//! Agent-level usage attribution for custom-provider pricing and context.
//!
//! These tests drive the real [`ZedAgent::publish_lody_usage`] path with a
//! synthetic response whose `model` field is empty, so the fallback model, the
//! resolved profile, and the serialized wire notification are all exercised
//! together rather than projected in isolation. The clocked cases drive the
//! same path with a stepped publication clock, so the span's elapsed duration
//! is a function of explicit inputs rather than of the wall clock.

use super::*;
use diapason_core::llm::Usage;
use rstest::rstest;
use serde_json::Value;
use tokio::sync::Notify;

const WIRE_PROVIDER: &str = "wire-test";

const WIRE_MODEL: &str = "wire-model";

const OTHER_MODEL: &str = "wire-other";

/// Provider with provider-default pricing and context that a model profile can
/// override for `WIRE_MODEL`.
fn wire_provider() -> CustomProviderConfig {
    let mut profiles = BTreeMap::new();
    let _ = profiles.insert(
        WIRE_MODEL.to_string(),
        diapason_config::core::CustomProviderProfileConfig {
            api_format: diapason_config::core::CustomProviderApiFormat::OpenAIChat,
            context_window: Some(32_768),
            pricing: diapason_config::core::CustomProviderPricingConfig {
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
        api_format: diapason_config::core::CustomProviderApiFormat::OpenAIChat,
        context_window: Some(200_000),
        pricing: diapason_config::core::CustomProviderPricingConfig {
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
    let vt_config = DiapasonConfig {
        custom_providers: vec![wire_provider()],
        ..DiapasonConfig::default()
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

    let agent_connection = install_handlers(Agent.builder().name("diapason-usage-profile-test"), Arc::clone(&agent))
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
    // Wait for the usage notification before releasing the client connection:
    // closing it first can drop the update before the handler forwards it.
    let first = tokio::time::timeout(Duration::from_secs(3), notifications_rx.recv())
        .await
        .expect("usage notification deadline")
        .expect("usage notification channel");
    release_client.notify_one();

    tokio::time::timeout(Duration::from_secs(3), client_task)
        .await
        .expect("usage client connection deadline")
        .expect("usage client task")
        .expect("usage client connection");
    agent_task.abort();
    drop(agent_task.await);

    let mut notifications = vec![first];
    notifications.extend(std::iter::from_fn(|| notifications_rx.try_recv().ok()));
    notifications
}

/// Interval the injected usage-publication clock reports, in milliseconds.
const USAGE_CLOCK_STEP_MS: u64 = 275;

/// Client state and expected outcome for one clocked publication case.
#[derive(Clone, Copy)]
enum ClockedOutcome {
    /// The client stays connected and receives the usage update.
    Delivered,
    /// The client stays connected, but the response carries no usage.
    NoUsage,
}

/// Publish one usage delta with a stepped publication clock.
///
/// Returns the extension notifications the client received, if any, and the
/// captured span records. Both clock reads in the publication path come from
/// the stepped clock, so the reported `elapsed_ms` is exactly
/// `USAGE_CLOCK_STEP_MS`. The send-failure path shares the same two clock
/// reads; it is covered by `lody_usage::tests` because a duplex peer cannot be
/// made to reject a notification from this harness.
async fn clocked_publication(response: LLMResponse, outcome: ClockedOutcome) -> (Vec<acp::AgentNotification>, String) {
    // Provider registration and the session archive directory are process
    // globals, so these tests serialize with the other prompt/provider tests.
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;

    let workspace = TempDir::new().expect("clocked usage workspace");
    let vt_config = DiapasonConfig {
        custom_providers: vec![wire_provider()],
        ..DiapasonConfig::default()
    };
    let startup =
        crate::zed::session::prepare_acp_agent_startup(&wire_core_config(workspace.path()), &vt_config, None, true)
            .await
            .expect("prepare ACP launch with the wire custom provider");
    let mut agent = crate::zed::session::create_acp_agent(startup)
        .await
        .expect("create ACP wire usage agent");
    agent.set_publication_clock(Arc::new(SteppedPublicationClock::new(Duration::from_millis(USAGE_CLOCK_STEP_MS))));
    let agent = Arc::new(agent);

    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();
    let client_ready = Arc::new(Notify::new());
    let release_client = Arc::new(Notify::new());

    let agent_connection = install_handlers(Agent.builder().name("diapason-usage-clock-test"), Arc::clone(&agent))
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

    let trace = TraceCapture::default();
    {
        let _guard = tracing::subscriber::set_default(Registry::default().with(trace.clone()));
        agent.publish_lody_usage(
            &acp::SessionId::new(Arc::from("session-usage-clock")),
            WIRE_PROVIDER,
            WIRE_MODEL,
            &response,
        );
    }

    let mut notifications = Vec::new();
    if matches!(outcome, ClockedOutcome::Delivered) {
        // Wait for the usage notification before releasing the client
        // connection: closing it first can drop the update.
        let first = tokio::time::timeout(Duration::from_secs(3), notifications_rx.recv())
            .await
            .expect("usage notification deadline")
            .expect("usage notification channel");
        notifications.push(first);
    }
    release_client.notify_one();
    tokio::time::timeout(Duration::from_secs(3), client_task)
        .await
        .expect("usage client connection deadline")
        .expect("usage client task")
        .expect("usage client connection");
    notifications.extend(std::iter::from_fn(|| notifications_rx.try_recv().ok()));

    agent_task.abort();
    drop(agent_task.await);

    let captured = trace.0.lock().expect("trace capture lock").join("\n");
    (notifications, captured)
}

/// A published delta reports the injected interval and the sent outcome.
#[tokio::test]
async fn clocked_publication_reports_the_injected_elapsed_on_success() {
    let response = LLMResponse {
        usage: Some(Usage {
            prompt_tokens: 7,
            completion_tokens: 3,
            ..Usage::default()
        }),
        ..LLMResponse::new(WIRE_MODEL, "answer")
    };

    // The duplex harness future is large enough to trip `large_futures`; pin it
    // on the heap rather than growing the test's stack frame.
    let (notifications, captured) = Box::pin(clocked_publication(response, ClockedOutcome::Delivered)).await;
    let update = usage_update(&notifications);

    assert_eq!(update["usage"]["inputTokens"], 7);
    assert!(captured.contains("outcome=\"sent\""), "the sent outcome must be recorded on the span: {captured}");
    assert!(
        captured.contains(&format!("elapsed_ms={USAGE_CLOCK_STEP_MS}")),
        "the send duration must be the injected interval, not the wall clock: {captured}"
    );
}

/// A response without usage reports the injected interval and `no_usage`.
#[tokio::test]
async fn clocked_publication_reports_the_injected_elapsed_without_usage() {
    let response = LLMResponse::new(WIRE_MODEL, "answer");

    let (notifications, captured) = Box::pin(clocked_publication(response, ClockedOutcome::NoUsage)).await;

    assert!(notifications.is_empty(), "a response without usage publishes nothing: {notifications:?}");
    assert!(
        captured.contains("outcome=\"no_usage\""),
        "the no-usage outcome must be recorded on the span: {captured}"
    );
    assert!(
        captured.contains(&format!("elapsed_ms={USAGE_CLOCK_STEP_MS}")),
        "the no-usage duration must be the injected interval, not the wall clock: {captured}"
    );
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

/// The session model's profile supplies the reported context window and cost,
/// and a model absent from the profile map inherits the provider defaults.
///
/// A response whose `model` field is empty must fall back to the session model,
/// so the usage delta is keyed by the model that actually served the turn.
#[rstest]
#[case::profiled_model(
    WIRE_MODEL,
    32_768,
    0.25e-6,
    1.00e-6,
    "the model profile must win over the provider-default context window"
)]
#[case::unprofiled_model(
    OTHER_MODEL,
    200_000,
    3.0e-6,
    15.0e-6,
    "an unprofiled model must inherit the provider-default context window"
)]
#[tokio::test]
async fn session_model_selects_the_reported_profile_and_pricing(
    #[case] session_model: &str,
    #[case] expected_context_window: u64,
    #[case] input_per_token_usd: f64,
    #[case] output_per_token_usd: f64,
    #[case] context_window_message: &str,
) {
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
    let notifications = Box::pin(published_usage_updates(session_model, response)).await;
    let update = usage_update(&notifications);

    assert_eq!(update["sessionId"], "session-usage-profile");
    assert_eq!(
        update["modelUsage"][session_model]["inputTokens"], 13,
        "the fallback session model must key the usage delta, not an empty model"
    );
    assert_eq!(update["usage"]["contextWindow"], expected_context_window, "{context_window_message}");
    let cost = update["usage"]["costUSD"].as_f64().expect("cost must be reported");
    let expected = 13.0 * input_per_token_usd + 5.0 * output_per_token_usd;
    assert!(
        (cost - expected).abs() < 1e-12,
        "the resolved profile must price the turn: expected {expected}, got {cost}"
    );
}
