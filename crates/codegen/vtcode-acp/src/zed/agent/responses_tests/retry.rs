//! Retry, checkpoint, and partial tool-input behaviour.
use super::super::provider::{StreamDeadlineTracker, StreamTimeoutPhase};
use super::super::tests::{
    PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers,
};
use super::super::*;
use super::support::*;
use crate::acp::{InitializeRequest, NewSessionRequest, SetSessionConfigOptionRequest};
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

/// Proves quota retries back off and then stream reasoning, text, quota, and usage over ACP.
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

    let run = run_acp_prompts(provider_config(server.uri()), &["Use the Responses route"], ACP_PROTOCOL_DEADLINE).await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn]);
    assert_eq!(run.active_acp_requests, 0, "Responses retry must release every provider permit");
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

/// Proves truncated JSON after partial text is checkpointed for the next request without automatic replay.
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
        ACP_PROTOCOL_DEADLINE,
    )
    .await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn, acp::StopReason::EndTurn]);
    assert_eq!(run.active_acp_requests, 0);
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

/// Proves partial custom tool input is previewed, never executed or retried, and the next prompt stays safe.
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
        ACP_PROTOCOL_DEADLINE,
    )
    .await;

    assert_eq!(run.stop_reasons, [acp::StopReason::EndTurn, acp::StopReason::EndTurn]);
    assert_eq!(run.active_acp_requests, 0);
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
