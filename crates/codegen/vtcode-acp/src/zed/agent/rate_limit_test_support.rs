//! Shared scripted ACP rate-limit test harness.

use super::tests::{PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers};
use super::*;
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::mpsc;
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

pub(super) const MODEL: &str = "wire-model";
pub(super) const PROVIDER: &str = "wire-test";

#[derive(Clone)]
pub(super) struct ScriptedResponder {
    pub(super) responses: Arc<[ResponseTemplate]>,
    pub(super) request_times: Arc<Mutex<Vec<StdInstant>>>,
    pub(super) next_response: Arc<AtomicUsize>,
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

pub(super) struct PromptRun {
    pub(super) initialized_lody: serde_json::Value,
    pub(super) notifications: Vec<acp::AgentNotification>,
    pub(super) request_times: Vec<StdInstant>,
    pub(super) visible_text: String,
}

pub(super) fn provider_config() -> CustomProviderConfig {
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

pub(super) fn custom_provider(config: CustomProviderConfig) -> CustomProviderBackendRouter {
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

pub(super) fn rate_limited_response(headers: &[(&str, &str)]) -> ResponseTemplate {
    let mut response =
        ResponseTemplate::new(429).set_body_json(serde_json::json!({"error": {"message": "fixture quota exhausted"}}));
    for &(name, value) in headers {
        response = response.insert_header(name, value);
    }
    response
}

pub(super) fn successful_stream_response() -> ResponseTemplate {
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

pub(super) async fn run_scripted_prompt(
    mut config: CustomProviderConfig,
    responses: Vec<ResponseTemplate>,
) -> PromptRun {
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

pub(super) fn visible_text(notifications: &[acp::AgentNotification]) -> String {
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

pub(super) fn notice_values(notifications: &[acp::AgentNotification]) -> Vec<serde_json::Value> {
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

pub(super) fn notice_messages(notifications: &[acp::AgentNotification]) -> Vec<String> {
    notice_values(notifications)
        .into_iter()
        .filter_map(|update| update["_meta"]["lody"]["notice"]["message"].as_str().map(str::to_string))
        .collect()
}

pub(super) fn rate_limit_snapshots(notifications: &[acp::AgentNotification]) -> Vec<serde_json::Value> {
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

pub(super) fn assert_request_floor(request_times: &[StdInstant], interval_index: usize, minimum: StdDuration) {
    assert!(
        request_times.len() > interval_index + 1,
        "need request pair at index {interval_index}; observed {} requests",
        request_times.len()
    );
    let actual = request_times[interval_index + 1].duration_since(request_times[interval_index]);
    assert!(actual >= minimum, "retry interval {interval_index} was {actual:?}, below floor {minimum:?}");
}
