//! Shared harness for custom OpenAI Responses ACP coverage.
//!
//! One ACP harness serves every behavioural submodule so retry, cancellation,
//! deadline, and gateway tests observe the same protocol and provider wiring.

use super::super::tests::{
    PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers,
};
use super::super::*;
use crate::acp::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use futures::stream;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant as StdInstant};
use tokio::sync::mpsc;
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_core::llm::provider::Message as ProviderMessage;
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::{Request, Respond, ResponseTemplate};

pub(super) const MODEL: &str = "responses-wire-model";
pub(super) const PROVIDER: &str = "wire-test";

/// Outer deadline for one ACP protocol flow.
///
/// These tests drive a real fixture server, agent, and client over a duplex
/// connection. The deadline is a hang guard, not a latency assertion, so it
/// stays well above scheduler and build-machine contention; behaviour is
/// decided by the protocol assertions, never by this clock.
pub(super) const ACP_PROTOCOL_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub(super) struct CapturedRequest {
    pub(super) path: String,
    pub(super) body: Value,
}

#[derive(Clone)]
pub(super) struct ScriptedResponder {
    pub(super) responses: Arc<[ResponseTemplate]>,
    pub(super) request_times: Arc<Mutex<Vec<StdInstant>>>,
    pub(super) requests: Arc<Mutex<Vec<CapturedRequest>>>,
    pub(super) next_response: Arc<AtomicUsize>,
}

impl Respond for ScriptedResponder {
    /// Record the request time and path, then return the next scripted response.
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.request_times
            .lock()
            .expect("Responses request times")
            .push(StdInstant::now());
        self.requests.lock().expect("Responses requests").push(CapturedRequest {
            path: request.url.path().to_string(),
            body: serde_json::from_slice(&request.body).expect("Responses request JSON"),
        });
        let response_index = self.next_response.fetch_add(1, Ordering::SeqCst);
        self.responses
            .get(response_index)
            .or_else(|| self.responses.last())
            .expect("scripted Responses responder must have a response")
            .clone()
    }
}

pub(super) struct AcpRun {
    pub(super) notifications: Vec<acp::AgentNotification>,
    pub(super) output_times: Vec<StdInstant>,
    pub(super) stop_reasons: Vec<acp::StopReason>,
    pub(super) messages: Vec<ProviderMessage>,
    pub(super) checkpoint_json: Option<String>,
    pub(super) active_acp_requests: usize,
}

/// Builds the custom Responses provider config used by the wire harness.
pub(super) fn provider_config(base_url: String) -> CustomProviderConfig {
    CustomProviderConfig {
        name: PROVIDER.to_string(),
        display_name: "Responses wire provider".to_string(),
        base_url,
        api_format: CustomProviderApiFormat::OpenAIResponses,
        supports_tools: Some(false),
        supports_reasoning: Some(true),
        model: MODEL.to_string(),
        models: vec![MODEL.to_string()],
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 20,
            retry_max_backoff_ms: 20,
            retry_jitter: false,
            max_in_flight_requests: Some(1),
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    }
}

/// Constructs the custom provider backend router for the fixture provider.
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

/// Drives the prompts over a duplex ACP connection and returns captured notifications and session state.
pub(super) async fn run_acp_prompts(config: CustomProviderConfig, prompts: &[&str], timeout: Duration) -> AcpRun {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("Responses ACP workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (notifications_tx, mut notifications_rx) = mpsc::unbounded_channel();
    let output_times = Arc::new(Mutex::new(Vec::new()));
    let observed_output_times = Arc::clone(&output_times);

    let agent_connection = install_handlers(Agent.builder().name("vtcode-responses-test"), Arc::clone(&agent))
        .connect_with(agent_channel, {
            let agent = Arc::clone(&agent);
            async move |cx: ConnectionTo<Client>| {
                agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
                std::future::pending::<agent_client_protocol::Result<()>>().await
            }
        });
    let agent_task = tokio::spawn(agent_connection);
    let prompt_texts = prompts.iter().map(|prompt| (*prompt).to_string()).collect::<Vec<_>>();
    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::AgentNotification, _cx| {
                if let acp::AgentNotification::SessionNotification(session) = &notification
                    && let acp::SessionUpdate::AgentMessageChunk(chunk) = &session.update
                    && let acp::ContentBlock::Text(text) = &chunk.content
                    && !text.text.is_empty()
                {
                    observed_output_times
                        .lock()
                        .expect("Responses output times")
                        .push(StdInstant::now());
                }
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
            Ok((session.session_id, stop_reasons))
        });

    let (session_id, stop_reasons) = tokio::time::timeout(timeout, client_connection)
        .await
        .expect("Responses ACP protocol deadline")
        .expect("Responses ACP protocol flow");
    agent_task.abort();
    drop(agent_task.await);

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
    let notifications = std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect();
    let output_times = output_times.lock().expect("Responses output times").clone();

    AcpRun {
        notifications,
        output_times,
        stop_reasons,
        messages,
        checkpoint_json,
        active_acp_requests,
    }
}

/// Builds a complete streamed Responses fixture with reasoning, text, and usage.
pub(super) fn responses_success() -> ResponseTemplate {
    let events = [
        json!({"type":"response.created","sequence_number":0,"response":{"id":"resp_wire","status":"in_progress"}}),
        // Compatible-provider reasoning parts are snapshots, including a
        // nonempty initial prefix. These are synthetic, not captured traffic.
        json!({"type":"response.reasoning_part.added","sequence_number":1,"item_id":"r","output_index":0,"content_index":0,"part":{"type":"reasoning_text","text":"hel"}}),
        json!({"type":"response.reasoning_text.delta","sequence_number":2,"item_id":"r","output_index":0,"content_index":0,"delta":"lo"}),
        json!({"type":"response.reasoning_text.done","sequence_number":3,"item_id":"r","output_index":0,"content_index":0,"text":"hello"}),
        json!({"type":"response.reasoning_part.done","sequence_number":4,"item_id":"r","output_index":0,"content_index":0,"part":{"type":"reasoning_text","text":"hello"}}),
        json!({"type":"response.output_text.delta","sequence_number":5,"delta":"one "}),
        json!({"type":"response.output_text.delta","sequence_number":6,"delta":"two"}),
        json!({
            "type":"response.completed",
            "sequence_number":7,
            "response":{
                "id":"resp_wire",
                "status":"completed",
                "model":MODEL,
                "output":[
                    {"type":"reasoning","summary":[{"type":"summary_text","text":"hello"}]},
                    {"type":"message","role":"assistant","content":[{"type":"output_text","text":"one two"}]}
                ],
                "usage":{
                    "input_tokens":13,
                    "output_tokens":7,
                    "total_tokens":20,
                    "output_tokens_details":{"reasoning_tokens":2}
                }
            }
        }),
    ];
    let body = events.iter().map(|event| format!("data: {event}\n\n")).collect::<String>();
    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Builds a minimal streamed Responses fixture that emits only the given text.
pub(super) fn responses_text_success(text: &str) -> ResponseTemplate {
    let delta = json!({"type":"response.output_text.delta","sequence_number":0,"delta":text});
    let completed = json!({
        "type":"response.completed",
        "sequence_number":1,
        "response":{
            "id":"resp_follow_up",
            "status":"completed",
            "model":MODEL,
            "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
            "usage":{"input_tokens":2,"output_tokens":1,"total_tokens":3}
        }
    });
    ResponseTemplate::new(200).set_body_raw(format!("data: {delta}\n\ndata: {completed}\n\n"), "text/event-stream")
}

/// Builds a 429 Responses reply carrying the given response headers.
pub(super) fn rate_limited_response(headers: &[(&str, &str)]) -> ResponseTemplate {
    let mut response =
        ResponseTemplate::new(429).set_body_json(json!({"error":{"message":"Responses quota exhausted"}}));
    for &(name, value) in headers {
        response = response.insert_header(name, value);
    }
    response
}

/// Collects the visible assistant message text from ACP notifications.
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

/// Collects the visible assistant reasoning text from ACP notifications.
pub(super) fn visible_reasoning(notifications: &[acp::AgentNotification]) -> String {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::SessionNotification(notification) => match &notification.update {
                acp::SessionUpdate::AgentThoughtChunk(chunk) => match &chunk.content {
                    acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Parses extension notifications whose method ends with the given suffix.
pub(super) fn ext_notifications(notifications: &[acp::AgentNotification], method_suffix: &str) -> Vec<Value> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::ExtNotification(notification)
                if notification.method.as_ref().trim_start_matches('_').ends_with(method_suffix) =>
            {
                serde_json::from_str(notification.params.get()).ok()
            }
            _ => None,
        })
        .collect()
}

/// Collects the text content of an ACP tool call.
pub(super) fn tool_content_text(content: &[acp::ToolCallContent]) -> String {
    content
        .iter()
        .filter_map(|content| match content {
            acp::ToolCallContent::Content(content) => match &content.content {
                acp::ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// Asserts every captured request used the streamed `/responses` route and fixture model.
pub(super) fn assert_responses_requests(requests: &[CapturedRequest], expected: usize) {
    assert_eq!(requests.len(), expected);
    for request in requests {
        assert_eq!(request.path, "/responses", "custom provider must not fall back to Chat Completions");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["model"], MODEL);
    }
}

#[derive(Clone)]
pub(super) struct DisconnectState {
    pub(super) requests: Arc<AtomicUsize>,
}
