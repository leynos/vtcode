//! Behavioural coverage for custom OpenAI Responses providers through ACP.

use super::tests::{PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers};
use super::*;
use agent_client_protocol::{Channel, on_receive_notification};
use assert_fs::TempDir;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{Response, StatusCode, header};
use axum::routing::post;
use futures::stream;
use serde_json::{Value, json};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration as StdDuration, Instant as StdInstant};
use tokio::sync::{Notify, mpsc};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderRequestPolicyConfig,
};
use vtcode_core::core::message_metadata::MessageMetadata;
use vtcode_core::llm::provider::{Message as ProviderMessage, MessageRole as ProviderMessageRole};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const MODEL: &str = "responses-wire-model";
const PROVIDER: &str = "wire-test";

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
    pub(super) active_permits: usize,
}

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

fn custom_provider(config: CustomProviderConfig) -> CustomProviderBackendRouter {
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

    let session = agent.session_handle(&session_id).expect("Responses ACP session");
    let (messages, checkpoint_path) = {
        let data = session.data.lock().expect("Responses session data");
        (data.thread.messages(), data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
    };
    let checkpoint_json = checkpoint_path.and_then(|path| std::fs::read_to_string(path).ok());
    let active_permits = agent
        .provider_runtime
        .for_provider(PROVIDER)
        .telemetry_snapshot()
        .active_permits;
    let notifications = std::iter::from_fn(|| notifications_rx.try_recv().ok()).collect();
    let output_times = output_times.lock().expect("Responses output times").clone();

    AcpRun {
        notifications,
        output_times,
        stop_reasons,
        messages,
        checkpoint_json,
        active_permits,
    }
}

fn responses_success() -> ResponseTemplate {
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

fn issue35_function_capture() -> String {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/friendli/issue35/function-response.sse");
    std::fs::read_to_string(path).expect("read sanitised issue 35 function-call capture")
}

fn issue35_safe_list_files_mutation() -> String {
    issue35_function_capture()
        .lines()
        .map(|line| {
            // Leave reasoning deltas and snapshots byte-identical: a global
            // name replacement would change only the unsplit done snapshot.
            let Some(payload) = line.strip_prefix("data: ") else {
                return line.to_string();
            };
            let mut event: Value = serde_json::from_str(payload).expect("captured SSE JSON");
            match event["type"].as_str() {
                Some("response.function_call_arguments.delta") => {
                    let delta = event["delta"].as_str().expect("argument delta");
                    event["delta"] = json!(delta.replace(r#"{"text": "OK"#, r#"{"path": ""#));
                }
                Some("response.function_call_arguments.done") => {
                    event["name"] = json!("list_files");
                    event["arguments"] = json!(r#"{"path": ""}"#);
                }
                Some("response.output_item.added" | "response.output_item.done") => {
                    mutate_issue35_function_item(&mut event["item"]);
                }
                Some("response.completed") => {
                    for item in event["response"]["output"].as_array_mut().expect("terminal output") {
                        mutate_issue35_function_item(item);
                    }
                }
                _ => {}
            }
            format!("data: {event}")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n\n"
}

fn mutate_issue35_function_item(item: &mut Value) {
    if item["type"] == "function_call" {
        item["name"] = json!("list_files");
        if item["arguments"].as_str().is_some_and(|arguments| !arguments.is_empty()) {
            item["arguments"] = json!(r#"{"path": ""}"#);
        }
    }
}

fn replace_last(input: &str, from: &str, to: &str) -> String {
    let (prefix, suffix) = input.rsplit_once(from).expect("issue 35 mutation source exists");
    format!("{prefix}{to}{suffix}")
}

fn issue35_missing_streamed_call_mutation() -> String {
    mutate_issue35_completed(issue35_safe_list_files_mutation(), |output| {
        output.retain(|item| item.get("type").and_then(Value::as_str) != Some("function_call"));
    })
}

fn issue35_ambiguous_streamed_call_mutation() -> String {
    let capture = issue35_safe_list_files_mutation();
    let duplicate = capture
        .split("\n\n")
        .filter(|event| {
            event.contains(r#""output_index":1"#)
                && (event.contains("response.output_item") || event.contains("response.function_call_arguments"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        .replace("a70ca8d6ea25b6e0", "a70ca8d6ea25b6ff")
        .replace("call_8b72964ed25d90d0", "call_8b72964ed25d9ff")
        .replace(r#""output_index":1"#, r#""output_index":2"#);
    let capture =
        capture.replacen("event: response.completed", &format!("{duplicate}\n\nevent: response.completed"), 1);
    let capture = mutate_issue35_completed(capture, |output| {
        let mut duplicate = output
            .iter()
            .find(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
            .expect("terminal function call")
            .clone();
        duplicate["id"] = json!("fc_864421b4b3a78bff");
        duplicate["call_id"] = json!("chatcmpl-tool-aec11d9791231fff");
        output.push(duplicate);
    });
    renumber_issue35_events(&capture)
}

fn mutate_issue35_completed(capture: String, mutate: impl FnOnce(&mut Vec<Value>)) -> String {
    let data_prefix = "data: ";
    let mut mutate = Some(mutate);
    capture
        .lines()
        .map(|line| {
            let Some(payload) = line.strip_prefix(data_prefix) else {
                return line.to_string();
            };
            let mut event: Value = serde_json::from_str(payload).expect("captured SSE JSON");
            if event.get("type").and_then(Value::as_str) != Some("response.completed") {
                return line.to_string();
            }
            let output = event["response"]["output"].as_array_mut().expect("terminal output array");
            mutate.take().expect("one completed event")(output);
            format!("{data_prefix}{event}")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n\n"
}

fn renumber_issue35_events(capture: &str) -> String {
    let mut sequence_number = 0_u64;
    capture
        .lines()
        .map(|line| {
            let Some(payload) = line.strip_prefix("data: ") else {
                return line.to_string();
            };
            let mut event: Value = serde_json::from_str(payload).expect("captured SSE JSON");
            event["sequence_number"] = json!(sequence_number);
            sequence_number += 1;
            format!("data: {event}")
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n\n"
}

fn executable_tool_call_ids(notifications: &[acp::AgentNotification]) -> Vec<String> {
    notifications
        .iter()
        .filter_map(|notification| match notification {
            acp::AgentNotification::SessionNotification(notification) => match &notification.update {
                acp::SessionUpdate::ToolCall(call) if !call.tool_call_id.0.starts_with("provider-input-preview:") => {
                    Some(call.tool_call_id.0.to_string())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn rate_limited_response(headers: &[(&str, &str)]) -> ResponseTemplate {
    let mut response =
        ResponseTemplate::new(429).set_body_json(json!({"error":{"message":"Responses quota exhausted"}}));
    for &(name, value) in headers {
        response = response.insert_header(name, value);
    }
    response
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

fn visible_reasoning(notifications: &[acp::AgentNotification]) -> String {
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

fn ext_notifications(notifications: &[acp::AgentNotification], method_suffix: &str) -> Vec<Value> {
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

fn tool_content_text(content: &[acp::ToolCallContent]) -> String {
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

fn assert_responses_requests(requests: &[CapturedRequest], expected: usize) {
    assert_eq!(requests.len(), expected);
    for request in requests {
        assert_eq!(request.path, "/responses", "custom provider must not fall back to Chat Completions");
        assert_eq!(request.body["stream"], true);
        assert_eq!(request.body["model"], MODEL);
    }
}

#[tokio::test]
async fn custom_responses_acp_retries_quota_then_streams_reasoning_text_and_usage() {
    let server = MockServer::start().await;
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![
                rate_limited_response(&[
                    ("retry-after", "0.03"),
                    ("x-ratelimit-limit-requests", "100"),
                    ("x-ratelimit-remaining-requests", "0"),
                ]),
                rate_limited_response(&[]),
                responses_success(),
            ]
            .into(),
            request_times: Arc::clone(&request_times),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(3)
        .mount(&server)
        .await;

    let run =
        run_acp_prompts(provider_config(server.uri()), &["Use the Responses route"], Duration::from_secs(3)).await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0, "Responses retry must release every provider permit");
    assert_eq!(visible_text(&run.notifications), "one two");
    assert_eq!(visible_reasoning(&run.notifications), "hello");
    let request_times = request_times.lock().expect("Responses request times");
    assert_eq!(request_times.len(), 3);
    assert!(request_times[1].duration_since(request_times[0]) >= StdDuration::from_millis(25));
    assert!(request_times[2].duration_since(request_times[1]) >= StdDuration::from_millis(55));
    assert_responses_requests(&requests.lock().expect("Responses requests"), 3);

    let rate_limits = ext_notifications(&run.notifications, "lody/rate_limits/update");
    assert_eq!(rate_limits.len(), 1, "only the response with complete quota values is publishable");
    assert_eq!(rate_limits[0]["rateLimits"][0]["windows"][0]["usedPercent"], 100.0);
    let usage = ext_notifications(&run.notifications, "lody/session/usage_update");
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0]["usage"]["inputTokens"], 13);
    assert_eq!(usage[0]["usage"]["outputTokens"], 5);
    assert_eq!(usage[0]["usage"]["reasoningOutputTokens"], 2);
}

#[tokio::test]
async fn partial_responses_truncated_json_is_checkpointed_without_automatic_replay() {
    let server = MockServer::start().await;
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let truncated = ResponseTemplate::new(200).set_body_raw(
        concat!(
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":0,\"delta\":\"partial answer\"}\n\n",
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":1,\"delta\":\n\n",
        ),
        "text/event-stream",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![truncated, responses_text_success("continued safely")].into(),
            request_times: Arc::clone(&request_times),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(2)
        .mount(&server)
        .await;

    let run = run_acp_prompts(
        provider_config(server.uri()),
        &["Start a partial answer", "Continue after the failed turn"],
        Duration::from_secs(3),
    )
    .await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn, acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0);
    assert_responses_requests(&requests.lock().expect("Responses requests"), 2);
    assert_eq!(request_times.lock().expect("Responses request times").len(), 2);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("partial answer"));
    assert!(visible.contains("You can retry the prompt"));
    assert!(visible.ends_with("continued safely"));
    let incomplete = run
        .messages
        .iter()
        .find(|message| {
            message.role == ProviderMessageRole::Assistant
                && message
                    .content
                    .as_text()
                    .split_once("\n\nThe provider could not complete this turn.")
                    .is_some_and(|(partial_content, _notice)| partial_content == "partial answer")
        })
        .expect("incomplete Responses assistant message");
    assert!(incomplete.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete));
    assert!(
        requests.lock().expect("Responses requests")[1]
            .body
            .to_string()
            .contains("partial answer"),
        "the next same-session request must retain the incomplete checkpoint"
    );
    assert!(
        run.checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("partial answer")),
        "the persisted ACP checkpoint must retain visible partial output"
    );
}

#[tokio::test]
async fn partial_custom_tool_input_is_never_executed_or_retried_and_next_prompt_is_safe() {
    let server = MockServer::start().await;
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let partial_tool_input = ResponseTemplate::new(200).set_body_raw(
        concat!(
            "data: {\"type\":\"response.output_item.added\",\"sequence_number\":0,\"item_id\":\"ct_1\",\"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ct_1\",\"call_id\":\"call_patch_1\",\"name\":\"apply_patch\",\"input\":\"\",\"status\":\"in_progress\"}}\n\n",
            "data: {\"type\":\"response.custom_tool_call_input.delta\",\"sequence_number\":1,\"item_id\":\"ct_1\",\"call_id\":\"call_patch_1\",\"output_index\":0,\"delta\":\"*** Begin Patch\\n*** Delete File: must-not-run\"}\n\n",
        ),
        "text/event-stream",
    );
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![partial_tool_input, responses_text_success("safe follow-up")].into(),
            request_times,
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut config = provider_config(server.uri());
    config.supports_tools = Some(true);

    let run = run_acp_prompts(
        config,
        &[
            "Start a tool input but do not finish it",
            "Answer safely after the interrupted tool input",
        ],
        Duration::from_secs(3),
    )
    .await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn, acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0);
    let requests = requests.lock().expect("Responses requests");
    assert_responses_requests(&requests, 2);
    assert!(requests[0].body["tools"].as_array().is_some_and(|tools| !tools.is_empty()));
    assert!(requests[1].body.to_string().contains("interrupted tool input"));
    assert!(
        !requests[1].body.to_string().contains("must-not-run"),
        "partial custom tool input must not enter the next request as executable history"
    );
    assert_eq!(visible_text(&run.notifications).matches("safe follow-up").count(), 1);
    let preview_id = "provider-input-preview:call_patch_1";
    let mut preview_started = false;
    let mut preview_delta_visible = false;
    let mut preview_failed = false;
    for notification in &run.notifications {
        let acp::AgentNotification::SessionNotification(notification) = notification else {
            continue;
        };
        match &notification.update {
            acp::SessionUpdate::ToolCall(call) => {
                assert_eq!(call.tool_call_id.0.as_ref(), preview_id, "no executable tool call may start");
                assert_eq!(call.status, acp::ToolCallStatus::Pending);
                assert!(call.title.contains("not executed"));
                preview_started = true;
            }
            acp::SessionUpdate::ToolCallUpdate(update) => {
                assert_eq!(update.tool_call_id.0.as_ref(), preview_id);
                preview_delta_visible |= update
                    .fields
                    .content
                    .as_deref()
                    .is_some_and(|content| tool_content_text(content).contains("must-not-run"));
                preview_failed |= update.fields.status == Some(acp::ToolCallStatus::Failed);
            }
            _ => {}
        }
    }
    assert!(preview_started, "raw custom-tool input must have a standard ACP pending preview");
    assert!(preview_delta_visible, "the pending preview must incrementally expose tool input");
    assert!(preview_failed, "an abandoned preview must terminate as failed before prompt completion");
}

#[tokio::test]
async fn friendli_function_id_mismatch_is_strict_by_default_and_never_executes() {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![ResponseTemplate::new(200).set_body_raw(issue35_function_capture(), "text/event-stream")]
                .into(),
            request_times: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut config = provider_config(server.uri());
    config.supports_tools = Some(true);

    let run = run_acp_prompts(config, &["Call fixture_echo exactly once with text OK"], Duration::from_secs(3)).await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_responses_requests(&requests.lock().expect("strict capture requests"), 1);
    assert!(executable_tool_call_ids(&run.notifications).is_empty());
    assert!(
        run.messages.iter().all(|message| !message.is_tool_response()),
        "strict mismatch must not execute or persist a tool result"
    );
    assert!(visible_text(&run.notifications).contains("completed response omitted a streamed tool call"));
}

#[tokio::test]
async fn friendli_function_id_remap_opt_in_executes_safe_tool_once_and_retains_final_id() {
    const STREAMED_CALL_ID: &str = "call_8b72964ed25d90d0";
    const FINAL_CALL_ID: &str = "chatcmpl-tool-aec11d97912311ee";

    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![
                ResponseTemplate::new(200).set_body_raw(issue35_safe_list_files_mutation(), "text/event-stream"),
                responses_text_success("safe tool completed"),
            ]
            .into(),
            request_times: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(2)
        .mount(&server)
        .await;
    let mut config = provider_config(server.uri());
    config.supports_tools = Some(true);
    config.responses_allow_function_call_id_remap = Some(true);

    let run = run_acp_prompts(config, &["List the current workspace once"], Duration::from_secs(3)).await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let requests = requests.lock().expect("opt-in capture requests");
    assert_eq!(requests.len(), 2, "ACP output: {}", visible_text(&run.notifications));
    assert_responses_requests(&requests, 2);
    let second_request = requests[1].body.to_string();
    assert_eq!(second_request.matches("function_call_output").count(), 1);
    assert!(second_request.contains(FINAL_CALL_ID));
    assert!(!second_request.contains(STREAMED_CALL_ID));
    assert_eq!(executable_tool_call_ids(&run.notifications), [FINAL_CALL_ID]);
    let durable_history = serde_json::to_string(&run.messages).expect("serialize durable response history");
    assert!(durable_history.contains(FINAL_CALL_ID));
    assert!(!durable_history.contains(STREAMED_CALL_ID));
}

#[tokio::test]
async fn friendli_function_id_remap_rejects_ambiguous_missing_and_changed_candidates() {
    let safe_capture = issue35_safe_list_files_mutation();
    let cases = [
        ("ambiguous", issue35_ambiguous_streamed_call_mutation(), "remapping is ambiguous"),
        ("missing", issue35_missing_streamed_call_mutation(), "do not map one-to-one to streamed calls"),
        (
            "changed_name",
            replace_last(&safe_capture, r#""name":"list_files""#, r#""name":"read_file""#),
            "contradict streamed calls",
        ),
        (
            "changed_arguments",
            replace_last(&safe_capture, r#"\"path\": \"\""#, r#"\"path\": \"different\""#),
            "contradict streamed calls",
        ),
    ];

    for (case, response, diagnostic) in cases {
        let server = MockServer::start().await;
        let requests = Arc::new(Mutex::new(Vec::new()));
        Mock::given(method("POST"))
            .and(path("/responses"))
            .respond_with(ScriptedResponder {
                responses: vec![ResponseTemplate::new(200).set_body_raw(response, "text/event-stream")].into(),
                request_times: Arc::new(Mutex::new(Vec::new())),
                requests: Arc::clone(&requests),
                next_response: Arc::new(AtomicUsize::new(0)),
            })
            .expect(1)
            .mount(&server)
            .await;
        let mut config = provider_config(server.uri());
        config.supports_tools = Some(true);
        config.responses_allow_function_call_id_remap = Some(true);

        let run = run_acp_prompts(config, &["List the current workspace once"], Duration::from_secs(3)).await;

        assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn], "{case}");
        assert_responses_requests(&requests.lock().expect("rejected remap requests"), 1);
        assert!(executable_tool_call_ids(&run.notifications).is_empty(), "{case}");
        assert!(run.messages.iter().all(|message| !message.is_tool_response()), "{case}");
        let visible = visible_text(&run.notifications);
        assert!(visible.contains(diagnostic), "{case}: expected {diagnostic:?}, received {visible:?}");
    }
}

#[tokio::test]
async fn interrupted_friendli_function_stream_checkpoints_without_tool_execution() {
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let interrupted = issue35_safe_list_files_mutation()
        .split("event: response.completed")
        .next()
        .expect("captured pre-terminal prefix")
        .to_string();
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![ResponseTemplate::new(200).set_body_raw(interrupted, "text/event-stream")].into(),
            request_times: Arc::new(Mutex::new(Vec::new())),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .expect(1)
        .mount(&server)
        .await;
    let mut config = provider_config(server.uri());
    config.supports_tools = Some(true);
    config.responses_allow_function_call_id_remap = Some(true);

    let run = run_acp_prompts(
        config,
        &["Begin one workspace listing but interrupt before the terminal response"],
        Duration::from_secs(3),
    )
    .await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_responses_requests(&requests.lock().expect("interrupted capture requests"), 1);
    assert!(executable_tool_call_ids(&run.notifications).is_empty());
    assert!(run.messages.iter().all(|message| !message.is_tool_response()));
    assert!(
        run.checkpoint_json
            .as_deref()
            .is_some_and(|checkpoint| checkpoint.contains("provider could not complete this turn")),
        "the interrupted stream must leave a durable incomplete checkpoint"
    );
}

#[tokio::test]
async fn cancelling_responses_retry_wait_stops_requests_output_and_releases_permit() {
    let server = MockServer::start().await;
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ScriptedResponder {
            responses: vec![
                rate_limited_response(&[("retry-after", "0.3")]),
                responses_text_success("must not appear"),
            ]
            .into(),
            request_times: Arc::clone(&request_times),
            requests: Arc::clone(&requests),
            next_response: Arc::new(AtomicUsize::new(0)),
        })
        .mount(&server)
        .await;

    let config = provider_config(server.uri());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("Responses cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let retry_notice = Arc::new(Notify::new());
    let callback_notice = Arc::clone(&retry_notice);
    let late_output = Arc::new(AtomicUsize::new(0));
    let observed_output = Arc::clone(&late_output);

    let agent_connection = install_handlers(Agent.builder().name("vtcode-responses-cancel-test"), Arc::clone(&agent))
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
                match notification {
                    acp::AgentNotification::SessionNotification(ref session)
                        if matches!(&session.update, acp::SessionUpdate::SessionInfoUpdate(_)) =>
                    {
                        callback_notice.notify_one();
                    }
                    acp::AgentNotification::SessionNotification(ref session)
                        if matches!(
                            &session.update,
                            acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_)
                        ) =>
                    {
                        let _previous_output_count = observed_output.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
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
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Cancel Responses retry"))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = retry_notice.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before retry notice: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(2), client_connection)
        .await
        .expect("cancelled Responses prompt deadline")
        .expect("cancelled Responses protocol flow");
    tokio::time::sleep(Duration::from_millis(350)).await;
    agent_task.abort();
    drop(agent_task.await);

    assert_responses_requests(&requests.lock().expect("Responses requests"), 1);
    assert_eq!(request_times.lock().expect("Responses request times").len(), 1);
    assert_eq!(late_output.load(Ordering::SeqCst), 0, "cancelled retry must emit no late model output");
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_permits,
        0,
        "cancellation must release the provider permit"
    );
}

#[derive(Clone)]
struct PendingResponsesState {
    requests: Arc<AtomicUsize>,
    request_started: Arc<Notify>,
}

async fn pending_responses(State(state): State<PendingResponsesState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    state.request_started.notify_one();
    std::future::pending().await
}

async fn pending_tool_input_responses(State(state): State<DisconnectState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let prefix = Bytes::from_static(
        concat!(
            "data: {\"type\":\"response.output_item.added\",\"sequence_number\":0,\"item_id\":\"ct_cancel\",\"output_index\":0,\"item\":{\"type\":\"custom_tool_call\",\"id\":\"ct_cancel\",\"call_id\":\"call_cancel\",\"name\":\"apply_patch\",\"input\":\"\",\"status\":\"in_progress\"}}\n\n",
            "data: {\"type\":\"response.custom_tool_call_input.delta\",\"sequence_number\":1,\"item_id\":\"ct_cancel\",\"call_id\":\"call_cancel\",\"output_index\":0,\"delta\":\"*** Begin Patch\\n*** Delete File: cancel-must-not-run\"}\n\n",
        )
        .as_bytes(),
    );
    let body = Body::from_stream(stream::unfold((0_u8, prefix), |(step, prefix)| async move {
        match step {
            0 => Some((Ok::<Bytes, std::io::Error>(prefix.clone()), (1, prefix))),
            _ => std::future::pending().await,
        }
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("pending tool-input Responses response")
}

#[tokio::test]
async fn cancelling_pending_responses_http_before_first_token_releases_permit_without_late_output() {
    let requests = Arc::new(AtomicUsize::new(0));
    let request_started = Arc::new(Notify::new());
    let app = Router::new()
        .route("/responses", post(pending_responses))
        .with_state(PendingResponsesState {
            requests: Arc::clone(&requests),
            request_started: Arc::clone(&request_started),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind pending Responses server");
    let address = listener.local_addr().expect("pending Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve pending Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.request_policy.max_retries = 0;
    config.request_policy.first_token_timeout_seconds = 3;

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("pending Responses cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let late_output = Arc::new(AtomicUsize::new(0));
    let observed_output = Arc::clone(&late_output);

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-responses-ttft-cancel-test"),
        Arc::clone(&agent),
    )
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
                if let acp::AgentNotification::SessionNotification(session) = notification
                    && matches!(
                        session.update,
                        acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_)
                    )
                {
                    let _previous_output_count = observed_output.fetch_add(1, Ordering::SeqCst);
                }
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
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Cancel pending Responses HTTP"))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = request_started.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before the HTTP request became pending: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(2), client_connection)
        .await
        .expect("pending Responses cancellation deadline")
        .expect("pending Responses cancellation protocol flow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent_task.abort();
    drop(agent_task.await);
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(late_output.load(Ordering::SeqCst), 0);
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_permits,
        0,
        "cancelling an in-flight Responses HTTP request must release the provider permit"
    );
}

#[tokio::test]
async fn cancelling_partial_tool_input_fails_preview_without_execution_retry_or_late_model_output() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(pending_tool_input_responses))
        .with_state(DisconnectState { requests: Arc::clone(&requests) });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind pending tool-input Responses server");
    let address = listener.local_addr().expect("pending tool-input Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app)
            .await
            .expect("serve pending tool-input Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.supports_tools = Some(true);

    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let factory_config = config.clone();
    let _factory_guard = PromptProviderFactoryGuard::install(
        PROVIDER,
        Arc::new(move || Box::new(custom_provider(factory_config.clone()))),
    );
    let workspace = TempDir::new().expect("tool-input cancellation workspace");
    let workspace_path = workspace.path().to_path_buf();
    let agent = Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[config]).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let preview_visible = Arc::new(Notify::new());
    let callback_preview_visible = Arc::clone(&preview_visible);
    let preview_statuses = Arc::new(Mutex::new(Vec::new()));
    let callback_statuses = Arc::clone(&preview_statuses);
    let late_model_output = Arc::new(AtomicUsize::new(0));
    let callback_model_output = Arc::clone(&late_model_output);

    let agent_connection = install_handlers(
        Agent.builder().name("vtcode-responses-tool-cancel-test"),
        Arc::clone(&agent),
    )
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
                if let acp::AgentNotification::SessionNotification(session) = notification {
                    match session.update {
                        acp::SessionUpdate::ToolCall(call) => {
                            callback_statuses.lock().expect("preview statuses").push((
                                call.tool_call_id.0.as_ref().to_string(),
                                Some(call.status),
                                tool_content_text(&call.content),
                            ));
                        }
                        acp::SessionUpdate::ToolCallUpdate(update) => {
                            let content = update.fields.content.as_deref().map(tool_content_text).unwrap_or_default();
                            if content.contains("cancel-must-not-run") {
                                callback_preview_visible.notify_one();
                            }
                            callback_statuses.lock().expect("preview statuses").push((
                                update.tool_call_id.0.as_ref().to_string(),
                                update.fields.status,
                                content,
                            ));
                        }
                        acp::SessionUpdate::AgentMessageChunk(_) | acp::SessionUpdate::AgentThoughtChunk(_) => {
                            let _previous_output_count = callback_model_output.fetch_add(1, Ordering::SeqCst);
                        }
                        _ => {}
                    }
                }
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
            let prompt = cx
                .send_request(PromptRequest::new(
                    session.session_id.clone(),
                    vec![acp::ContentBlock::Text(acp::TextContent::new(
                        "Cancel partial tool input",
                    ))],
                ))
                .block_task();
            tokio::pin!(prompt);
            tokio::select! {
                () = preview_visible.notified() => {}
                result = &mut prompt => panic!("Responses prompt finished before tool preview: {result:?}"),
            }
            cx.send_notification(CancelNotification::new(session.session_id))?;
            let response = prompt.await?;
            assert_eq!(response.stop_reason, acp::StopReason::Cancelled);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(2), client_connection)
        .await
        .expect("tool-input cancellation deadline")
        .expect("tool-input cancellation protocol flow");
    tokio::time::sleep(Duration::from_millis(50)).await;
    agent_task.abort();
    drop(agent_task.await);
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1);
    assert_eq!(late_model_output.load(Ordering::SeqCst), 0);
    assert_eq!(
        agent
            .provider_runtime
            .for_provider(PROVIDER)
            .telemetry_snapshot()
            .active_permits,
        0
    );
    let preview_statuses = preview_statuses.lock().expect("preview statuses");
    assert!(!preview_statuses.is_empty());
    assert!(
        preview_statuses
            .iter()
            .all(|(id, _, _)| id == "provider-input-preview:call_cancel")
    );
    assert!(preview_statuses.iter().any(|(_, status, content)| {
        *status == Some(acp::ToolCallStatus::Pending) && content.contains("cancel-must-not-run")
    }));
    assert!(
        preview_statuses
            .iter()
            .any(|(_, status, _)| *status == Some(acp::ToolCallStatus::Failed))
    );
}

#[derive(Clone)]
struct DisconnectState {
    requests: Arc<AtomicUsize>,
}

async fn partial_disconnect(State(state): State<DisconnectState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let first = Bytes::from_static(
        b"data: {\"type\":\"response.output_text.delta\",\"sequence_number\":0,\"delta\":\"visible before disconnect\"}\n\n",
    );
    let body = Body::from_stream(stream::unfold((0_u8, first), |(step, first)| async move {
        match step {
            0 => Some((Ok::<Bytes, std::io::Error>(first.clone()), (1, first))),
            1 => {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Some((Err(std::io::Error::other("scripted Responses disconnect")), (2, first)))
            }
            _ => None,
        }
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("partial disconnect response")
}

#[tokio::test]
async fn partial_responses_disconnect_is_not_retried_and_marks_turn_incomplete() {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(partial_disconnect))
        .with_state(DisconnectState { requests: Arc::clone(&requests) });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind disconnect server");
    let address = listener.local_addr().expect("disconnect server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve disconnect fixture");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Disconnect after visible output"],
        Duration::from_secs(3),
    )
    .await;
    server_task.abort();
    drop(server_task.await);

    assert_eq!(requests.load(Ordering::SeqCst), 1, "visible output makes automatic replay unsafe");
    assert_eq!(run.active_permits, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert!(visible_text(&run.notifications).contains("visible before disconnect"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("visible before disconnect")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}

#[derive(Clone)]
struct PacedResponsesState {
    frames: Arc<[(StdDuration, Bytes)]>,
    requests: Arc<AtomicUsize>,
}

async fn paced_responses(State(state): State<PacedResponsesState>, _body: Bytes) -> Response<Body> {
    let _previous_request_count = state.requests.fetch_add(1, Ordering::SeqCst);
    let body = Body::from_stream(stream::unfold((0_usize, state.frames), |(frame_index, frames)| async move {
        let (delay, frame) = frames.get(frame_index)?.clone();
        tokio::time::sleep(delay).await;
        Some((Ok::<Bytes, std::io::Error>(frame), (frame_index + 1, frames)))
    }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(body)
        .expect("paced Responses response")
}

async fn run_paced_responses(
    frames: Vec<(StdDuration, Bytes)>,
    first_token_seconds: u64,
    idle_seconds: u64,
    total_seconds: u64,
) -> (AcpRun, usize) {
    let requests = Arc::new(AtomicUsize::new(0));
    let app = Router::new()
        .route("/responses", post(paced_responses))
        .with_state(PacedResponsesState {
            frames: frames.into(),
            requests: Arc::clone(&requests),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind paced Responses server");
    let address = listener.local_addr().expect("paced Responses server address");
    let server_task = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve paced Responses fixture");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.request_policy.max_retries = 0;
    config.request_policy.first_token_timeout_seconds = first_token_seconds;
    config.request_policy.stream_idle_timeout_seconds = idle_seconds;
    config.request_policy.total_generation_timeout_seconds = total_seconds;

    let run = run_acp_prompts(config, &["Exercise Responses stream deadlines"], Duration::from_secs(4)).await;
    server_task.abort();
    drop(server_task.await);
    (run, requests.load(Ordering::SeqCst))
}

fn responses_delta(text: &str) -> Bytes {
    Bytes::from(format!("data: {}\n\n", json!({"type":"response.output_text.delta","delta":text})))
}

fn responses_completed(text: &str) -> Bytes {
    Bytes::from(format!(
        "data: {}\n\n",
        json!({
            "type":"response.completed",
            "response":{
                "id":"resp_timed",
                "status":"completed",
                "model":MODEL,
                "output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]}],
                "usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}
            }
        })
    ))
}

#[test]
fn responses_output_progress_updates_exact_idle_boundary_without_sliding_total_deadline() {
    let started_at = Instant::now();
    let total_deadline = started_at + Duration::from_secs(10);
    let mut tracker = StreamDeadlineTracker::new(
        ProviderDeadlinePolicy {
            connect: None,
            first_token: Some(Duration::from_secs(5)),
            stream_idle: Some(Duration::from_secs(3)),
            total_generation: Some(Duration::from_secs(10)),
        },
        started_at,
    );

    // Responses lifecycle/status frames do not call `observe_output_at`; only
    // model text, reasoning, or tool-input progress moves these deadlines.
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::FirstToken, started_at + Duration::from_secs(5))));
    tracker.observe_output_at(started_at + Duration::from_secs(5));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::InterTokenIdle, started_at + Duration::from_secs(8))));
    tracker.observe_output_at(started_at + Duration::from_secs(8));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
    tracker.observe_output_at(started_at + Duration::from_secs(9));
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
    assert_eq!(tracker.total, Some(total_deadline));
}

#[test]
fn responses_healthy_trickle_keeps_idle_open_but_still_preserves_total_budget() {
    let started_at = Instant::now();
    let total_deadline = started_at + Duration::from_secs(20);
    let mut tracker = StreamDeadlineTracker::new(
        ProviderDeadlinePolicy {
            connect: None,
            first_token: Some(Duration::from_secs(5)),
            stream_idle: Some(Duration::from_secs(4)),
            total_generation: Some(Duration::from_secs(20)),
        },
        started_at,
    );

    for elapsed_seconds in [4, 7, 10, 13, 16, 17] {
        let progress_at = started_at + Duration::from_secs(elapsed_seconds);
        tracker.observe_output_at(progress_at);
        let (_phase, next_deadline) = tracker.next().expect("healthy Responses trickle deadline");
        assert!(next_deadline > progress_at);
        assert_eq!(tracker.total, Some(total_deadline));
    }
    assert_eq!(tracker.next(), Some((StreamTimeoutPhase::TotalGeneration, total_deadline)));
}

#[tokio::test]
async fn custom_responses_enforces_first_token_deadline_without_replay_or_permit_leak() {
    let (run, requests) =
        run_paced_responses(vec![(StdDuration::from_millis(1_100), responses_delta("too late"))], 1, 3, 3).await;

    assert_eq!(requests, 1);
    assert_eq!(run.active_permits, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("time to first token"));
    assert!(!visible.contains("too late"));
}

#[tokio::test]
async fn custom_responses_enforces_idle_deadline_after_partial_output_without_replay() {
    let (run, requests) = run_paced_responses(
        vec![
            (StdDuration::from_millis(10), responses_delta("idle prefix")),
            (StdDuration::from_millis(1_100), responses_completed("idle prefix")),
        ],
        2,
        1,
        3,
    )
    .await;

    assert_eq!(requests, 1, "visible Responses output makes timeout replay unsafe");
    assert_eq!(run.active_permits, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("idle prefix"));
    assert!(visible.contains("inter-token idle"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("idle prefix")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}

#[tokio::test]
async fn custom_responses_total_deadline_is_not_extended_by_paced_output() {
    let (run, requests) = run_paced_responses(
        vec![
            (StdDuration::from_millis(100), responses_delta("one ")),
            (StdDuration::from_millis(600), responses_delta("two ")),
            (StdDuration::from_millis(600), responses_completed("one two")),
        ],
        1,
        1,
        1,
    )
    .await;

    assert_eq!(requests, 1, "total timeout after visible output must not replay Responses");
    assert_eq!(run.active_permits, 0);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    let visible = visible_text(&run.notifications);
    assert!(visible.contains("one two"));
    assert!(visible.contains("total generation"));
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && message.content.as_text().contains("one two")
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}

pub(super) struct RunningVidaiMock {
    child: Child,
    pub(super) responses_url: String,
}

impl RunningVidaiMock {
    pub(super) fn start(scenario: &str, provider_path: &str) -> anyhow::Result<Self> {
        let version = Command::new("vidaimock").arg("--version").output()?;
        anyhow::ensure!(version.status.success(), "vidaimock --version failed");
        anyhow::ensure!(
            String::from_utf8_lossy(&version.stdout).contains("0.3.1"),
            "Responses physics fixtures require VidaiMock 0.3.1"
        );
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let fixtures = vidaimock_fixture_root();
        let mut child = Command::new("vidaimock")
            .env("VIDAIMOCK_ISOLATED", "true")
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("--config")
            .arg(fixtures.join("scenarios").join(scenario))
            .arg("--config-dir")
            .arg(fixtures)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let address = ("127.0.0.1", port);
        for _ in 0..100 {
            if TcpStream::connect(address).is_ok() {
                return Ok(Self {
                    child,
                    responses_url: format!("http://127.0.0.1:{port}/v1/{provider_path}/responses"),
                });
            }
            if let Some(status) = child.try_wait()? {
                anyhow::bail!("VidaiMock exited before accepting Responses requests: {status}");
            }
            thread::sleep(StdDuration::from_millis(25));
        }
        drop(child.kill());
        drop(child.wait());
        anyhow::bail!("VidaiMock did not accept Responses requests within 2.5 seconds")
    }
}

impl Drop for RunningVidaiMock {
    fn drop(&mut self) {
        drop(self.child.kill());
        drop(self.child.wait());
    }
}

fn vidaimock_fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tests/fixtures/vidaimock")
}

#[derive(Clone)]
pub(super) struct ResponsesGatewayState {
    pub(super) upstream_responses_url: String,
    pub(super) paths: Arc<Mutex<Vec<String>>>,
    pub(super) attempts: Arc<AtomicUsize>,
    pub(super) quota_failures: usize,
    pub(super) request_times: Arc<Mutex<Vec<StdInstant>>>,
    pub(super) client: reqwest::Client,
}

pub(super) async fn responses_gateway(State(state): State<ResponsesGatewayState>, body: Bytes) -> Response<Body> {
    state
        .paths
        .lock()
        .expect("Responses gateway paths")
        .push("/responses".to_string());
    state
        .request_times
        .lock()
        .expect("Responses gateway request times")
        .push(StdInstant::now());
    if state.attempts.fetch_add(1, Ordering::SeqCst) < state.quota_failures {
        return Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "0.08")
            .header("x-ratelimit-limit-requests", "100")
            .header("x-ratelimit-remaining-requests", "0")
            .body(Body::from(r#"{"error":{"message":"Responses physics quota exhausted"}}"#))
            .expect("Responses quota gateway response");
    }
    match state
        .client
        .post(&state.upstream_responses_url)
        .header(header::CONTENT_TYPE.as_str(), "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(upstream) => {
            let status = upstream.status();
            let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
            let mut response = Response::builder().status(status);
            if let Some(content_type) = content_type {
                response = response.header(header::CONTENT_TYPE, content_type);
            }
            response
                .body(Body::from_stream(upstream.bytes_stream()))
                .expect("proxied Responses body")
        }
        Err(error) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("VidaiMock Responses proxy failure: {error}")))
            .expect("VidaiMock Responses proxy error"),
    }
}

async fn function_remap_gateway(State(state): State<ResponsesGatewayState>, body: Bytes) -> Response<Body> {
    state
        .paths
        .lock()
        .expect("function remap gateway paths")
        .push("/responses".to_string());
    let attempt = state.attempts.fetch_add(1, Ordering::SeqCst);
    if attempt > 0 {
        let terminal = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"sequence_number\":0,\"output_index\":0,\"content_index\":0,\"delta\":\"safe tool completed\"}\n\n",
            "data: {\"type\":\"response.completed\",\"sequence_number\":1,\"response\":{\"id\":\"resp-after-tool\",\"object\":\"response\",\"status\":\"completed\",\"model\":\"responses-wire-model\",\"output\":[{\"id\":\"msg-after-tool\",\"type\":\"message\",\"role\":\"assistant\",\"status\":\"completed\",\"content\":[{\"type\":\"output_text\",\"text\":\"safe tool completed\",\"annotations\":[]}]}]}}\n\n",
        );
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(terminal))
            .expect("function remap terminal response");
    }
    match state
        .client
        .post(&state.upstream_responses_url)
        .header(header::CONTENT_TYPE.as_str(), "application/json")
        .body(body)
        .send()
        .await
    {
        Ok(upstream) => Response::builder()
            .status(upstream.status())
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from_stream(upstream.bytes_stream()))
            .expect("proxied function remap response"),
        Err(error) => Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::from(format!("VidaiMock function remap proxy failure: {error}")))
            .expect("VidaiMock function remap proxy error"),
    }
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates function-call remap physics"]
async fn vidaimock_function_id_remap_executes_only_after_terminal_completion() {
    const FINAL_CALL_ID: &str = "chatcmpl-tool-aec11d97912311ee";

    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-function-remap")
        .expect("start function remap VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let attempts = Arc::new(AtomicUsize::new(0));
    let gateway = Router::new()
        .route("/responses", post(function_remap_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::clone(&attempts),
            quota_failures: 0,
            request_times: Arc::new(Mutex::new(Vec::new())),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind function remap gateway");
    let address = listener.local_addr().expect("function remap gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve function remap gateway");
    });
    let mut config = provider_config(format!("http://{address}"));
    config.supports_tools = Some(true);
    config.responses_allow_function_call_id_remap = Some(true);

    let run = run_acp_prompts(config, &["List the workspace once"], Duration::from_secs(6)).await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(attempts.load(Ordering::SeqCst), 2, "one tool response plus one final response");
    assert_eq!(paths.lock().expect("function remap paths").len(), 2);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(executable_tool_call_ids(&run.notifications), [FINAL_CALL_ID]);
    assert_eq!(visible_text(&run.notifications).matches("safe tool completed").count(), 1);
    let durable_history = serde_json::to_string(&run.messages).expect("serialize VidaiMock tool history");
    assert!(durable_history.contains(FINAL_CALL_ID));
    assert!(!durable_history.contains("call_8b72964ed25d90d0"));
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates real Responses streaming physics"]
async fn vidaimock_custom_responses_crosses_acp_with_paced_output_and_usage() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-physics")
        .expect("start Responses VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let request_times = Arc::new(Mutex::new(Vec::new()));
    let gateway = Router::new()
        .route("/responses", post(responses_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::new(AtomicUsize::new(0)),
            quota_failures: 1,
            request_times: Arc::clone(&request_times),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind Responses gateway");
    let address = listener.local_addr().expect("Responses gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway).await.expect("serve Responses gateway");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Emit the Responses physics fixture"],
        Duration::from_secs(6),
    )
    .await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(paths.lock().expect("Responses gateway paths").as_slice(), ["/responses", "/responses"]);
    let request_times = request_times.lock().expect("Responses gateway request times");
    assert_eq!(request_times.len(), 2);
    assert!(request_times[1].duration_since(request_times[0]) >= StdDuration::from_millis(75));
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0);
    assert_eq!(
        visible_text(&run.notifications).split_whitespace().collect::<Vec<_>>(),
        ["one", "two", "three", "four"]
    );
    assert_eq!(visible_reasoning(&run.notifications), "fixture reasoning");
    let rate_limits = ext_notifications(&run.notifications, "lody/rate_limits/update");
    assert_eq!(rate_limits.len(), 1, "the Responses gateway 429 headers must reach ACP");
    assert_eq!(rate_limits[0]["rateLimits"][0]["windows"][0]["usedPercent"], 100.0);
    assert!(run.output_times.len() >= 4, "expected four paced Responses output chunks");
    for pair in run.output_times.windows(2).take(3) {
        assert!(
            pair[1].duration_since(pair[0]) >= StdDuration::from_millis(200),
            "VidaiMock Responses scenario did not preserve the configured 250 ms cadence"
        );
    }
    let usage = ext_notifications(&run.notifications, "lody/session/usage_update");
    assert_eq!(usage.len(), 1, "Responses terminal usage must reach ACP");
    assert_eq!(usage[0]["usage"]["inputTokens"], 13);
    assert_eq!(usage[0]["usage"]["outputTokens"], 5);
    assert_eq!(usage[0]["usage"]["reasoningOutputTokens"], 2);
}

#[tokio::test]
#[ignore = "requires pinned VidaiMock 0.3.1 and validates a real truncated Responses stream"]
async fn vidaimock_responses_truncated_stream_is_visible_incomplete_and_not_retried() {
    let vidaimock = RunningVidaiMock::start("trickled-stream.toml", "responses-physics-truncated")
        .expect("start truncating Responses VidaiMock fixture");
    let paths = Arc::new(Mutex::new(Vec::new()));
    let gateway = Router::new()
        .route("/responses", post(responses_gateway))
        .with_state(ResponsesGatewayState {
            upstream_responses_url: vidaimock.responses_url.clone(),
            paths: Arc::clone(&paths),
            attempts: Arc::new(AtomicUsize::new(0)),
            quota_failures: 0,
            request_times: Arc::new(Mutex::new(Vec::new())),
            client: reqwest::Client::new(),
        });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("bind truncating Responses gateway");
    let address = listener.local_addr().expect("truncating Responses gateway address");
    let gateway_task = tokio::spawn(async move {
        axum::serve(listener, gateway)
            .await
            .expect("serve truncating Responses gateway");
    });

    let run = run_acp_prompts(
        provider_config(format!("http://{address}")),
        &["Emit output before the Responses connection is cut"],
        Duration::from_secs(4),
    )
    .await;
    gateway_task.abort();
    drop(gateway_task.await);

    assert_eq!(paths.lock().expect("Responses gateway paths").as_slice(), ["/responses"]);
    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_permits, 0);
    assert!(!visible_text(&run.notifications).trim().is_empty());
    assert!(run.messages.iter().any(|message| {
        message.role == ProviderMessageRole::Assistant
            && !message.content.as_text().trim().is_empty()
            && message.metadata.as_ref().is_some_and(MessageMetadata::is_incomplete)
    }));
}
