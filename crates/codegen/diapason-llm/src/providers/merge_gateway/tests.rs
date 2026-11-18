//! Tests for the native Merge Gateway request and stream paths.

use super::*;
use crate::provider::{LLMProvider, LLMRequest, Message, NormalizedStreamEvent, ToolCall, ToolChoice};
use diapason_config::TimeoutsConfig;
use diapason_config::constants::models;
use diapason_utility_tool_specs::{apply_patch_parameters, write_stdin_parameters};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn test_provider(base_url: &str) -> MergeGatewayProvider {
    MergeGatewayProvider::new_with_client(
        "test-key".to_string(),
        models::merge_gateway::DEFAULT_ROUTING.to_string(),
        HttpClient::new(),
        base_url.to_string(),
        TimeoutsConfig::default(),
    )
}

fn sse(event: &str, data: Value) -> String {
    format!("event: {event}\ndata: {}\n\n", serde_json::to_string(&data).expect("event payload"))
}

fn sse_data(data: Value) -> String {
    format!("data: {}\n\n", serde_json::to_string(&data).expect("event payload"))
}

#[test]
fn native_payload_serializes_message_tool_use_and_tool_result_items() {
    let provider =
        MergeGatewayProvider::with_model("test-key".to_string(), models::merge_gateway::DEFAULT_ROUTING.to_string());
    let request = LLMRequest {
        system_prompt: Some(Arc::from("You are helpful")),
        messages: vec![
            Message::user("hello".to_string()),
            Message::assistant_with_tools(
                "calling tool".to_string(),
                vec![ToolCall::function(
                    "call_1".to_string(),
                    "get_weather".to_string(),
                    r#"{"location":"Paris"}"#.to_string(),
                )],
            ),
            Message::tool_response("call_1".to_string(), "sunny".to_string()),
        ]
        .into(),
        tools: Some(Arc::new(vec![ToolDefinition::function(
            "get_weather".to_string(),
            "Get weather".to_string(),
            json!({
                "type": "object",
                "properties": {
                    "location": {"type": "string"}
                },
                "required": ["location"]
            }),
        )])),
        model: models::merge_gateway::DEFAULT_ROUTING.to_string(),
        max_tokens: Some(128),
        temperature: Some(0.2),
        top_p: Some(0.9),
        stop_sequences: Some(vec!["END".to_string()]),
        tool_choice: Some(ToolChoice::Auto),
        output_format: Some(json!({
            "type": "json_schema",
            "json_schema": {
                "name": "weather",
                "schema": {"type": "object"}
            }
        })),
        service_tier: Some("flex".to_string()),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");
    let input = payload["input"].as_array().expect("input array");

    assert_eq!(payload["model"], models::merge_gateway::DEFAULT_ROUTING);
    assert_eq!(payload["max_tokens"], 128);
    assert!((payload["temperature"].as_f64().expect("temperature should be numeric") - 0.2).abs() < 1e-6);
    assert!((payload["top_p"].as_f64().expect("top_p should be numeric") - 0.9).abs() < 1e-6);
    assert_eq!(payload["stop"], json!(["END"]));
    assert_eq!(payload["tool_choice"], json!("auto"));
    assert_eq!(payload["response_format"]["type"], "json_schema");
    assert_eq!(payload["service_tier"], "flex");
    assert_eq!(payload["tools"][0]["type"], "function");
    assert_eq!(payload["tools"][0]["name"], "get_weather");
    assert_eq!(payload["tools"][0]["parameters"]["required"], json!(["location"]));
    assert_eq!(input.len(), 4);
    assert_eq!(input[0]["type"], "message");
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[0]["content"], "You are helpful");
    assert_eq!(input[1]["type"], "message");
    assert_eq!(input[1]["role"], "user");
    assert_eq!(input[1]["content"], "hello");
    assert_eq!(input[2]["type"], "message");
    assert_eq!(input[2]["role"], "assistant");
    assert!(input[2]["content"].is_array());
    assert_eq!(input[2]["content"][0]["type"], "text");
    assert_eq!(input[2]["content"][1]["type"], "tool_use");
    assert_eq!(input[2]["content"][1]["id"], "call_1");
    assert_eq!(input[2]["content"][1]["name"], "get_weather");
    assert_eq!(input[2]["content"][1]["input"], json!({"location": "Paris"}));
    assert_eq!(input[3]["type"], "tool_result");
    assert_eq!(input[3]["tool_use_id"], "call_1");
    assert_eq!(input[3]["content"], "sunny");
}

#[test]
fn native_payload_enables_response_streaming() {
    let provider =
        MergeGatewayProvider::with_model("test-key".to_string(), models::merge_gateway::XAI_GROK_4_6.to_string());
    let request = LLMRequest {
        model: models::merge_gateway::XAI_GROK_4_6.to_string(),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, true).expect("payload");

    assert_eq!(payload["stream"], true);
}

#[test]
fn native_payload_forwards_reasoning_effort_on_native_routes() {
    let provider =
        MergeGatewayProvider::with_model("test-key".to_string(), models::merge_gateway::OPENAI_GPT_5_5.to_string());
    let request = LLMRequest {
        model: models::merge_gateway::OPENAI_GPT_5_5.to_string(),
        reasoning_effort: Some(diapason_config::types::ReasoningEffortLevel::High),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");

    assert_eq!(payload["reasoning_effort"], "high");
    assert!(payload.get("thinking").is_none());
}

#[test]
fn native_payload_forwards_thinking_budget_for_budget_routes() {
    let provider = MergeGatewayProvider::with_model(
        "test-key".to_string(),
        models::merge_gateway::ANTHROPIC_CLAUDE_OPUS_5.to_string(),
    );
    let request = LLMRequest {
        model: models::merge_gateway::ANTHROPIC_CLAUDE_OPUS_5.to_string(),
        reasoning_effort: Some(diapason_config::types::ReasoningEffortLevel::High),
        max_tokens: Some(2000),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");

    assert!(payload.get("reasoning_effort").is_none());
    assert_eq!(payload["thinking"]["type"], "enabled");
    assert_eq!(payload["thinking"]["budget_tokens"], 1900);
}

#[test]
fn native_payload_omits_thinking_when_budget_exceeds_max_tokens() {
    let provider = MergeGatewayProvider::with_model(
        "test-key".to_string(),
        models::merge_gateway::DEEPSEEK_V4_PRO_0813.to_string(),
    );
    let request = LLMRequest {
        model: models::merge_gateway::DEEPSEEK_V4_PRO_0813.to_string(),
        reasoning_effort: Some(diapason_config::types::ReasoningEffortLevel::Medium),
        max_tokens: Some(1000),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");

    assert!(payload.get("reasoning_effort").is_none());
    assert!(payload.get("thinking").is_none());
}

#[test]
fn native_payload_omits_reasoning_for_unclassified_routes() {
    let provider =
        MergeGatewayProvider::with_model("test-key".to_string(), models::merge_gateway::DEFAULT_ROUTING.to_string());
    let request = LLMRequest {
        model: models::merge_gateway::DEFAULT_ROUTING.to_string(),
        reasoning_effort: Some(diapason_config::types::ReasoningEffortLevel::High),
        messages: vec![Message::user("hello".to_string())].into(),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");

    assert!(payload.get("reasoning_effort").is_none());
    assert!(payload.get("thinking").is_none());
}

#[test]
fn reasoning_capabilities_are_route_aware() {
    let provider =
        MergeGatewayProvider::with_model("test-key".to_string(), models::merge_gateway::OPENAI_GPT_5_5.to_string());
    assert!(provider.supports_reasoning(models::merge_gateway::OPENAI_GPT_5_5));
    assert!(provider.supports_reasoning_effort(models::merge_gateway::OPENAI_GPT_5_5));
    assert!(provider.supports_reasoning(models::merge_gateway::ANTHROPIC_CLAUDE_OPUS_5));
    assert!(provider.supports_reasoning_effort(models::merge_gateway::ANTHROPIC_CLAUDE_OPUS_5));
    assert!(!provider.supports_reasoning(models::merge_gateway::DEFAULT_ROUTING));
    assert!(!provider.supports_reasoning_effort(models::merge_gateway::DEFAULT_ROUTING));
}

#[test]
fn model_behaviour_override_wins_for_reasoning_capabilities() {
    let model = models::merge_gateway::ANTHROPIC_CLAUDE_OPUS_5.to_string();
    let model_behaviour = serde_json::from_value::<ModelConfig>(json!({
        "model_supports_reasoning": true,
        "model_supports_reasoning_effort": true,
    }))
    .expect("model behaviour");
    let provider = MergeGatewayProvider::from_config(
        Some("test-key".to_string()),
        Some(model.clone()),
        None,
        None,
        None,
        None,
        Some(model_behaviour),
    );

    assert!(provider.supports_reasoning(&model));
    assert!(provider.supports_reasoning_effort(&model));
}

#[test]
fn native_payload_sanitizes_gemini_incompatible_tool_schemas() {
    let provider = MergeGatewayProvider::with_model(
        "test-key".to_string(),
        models::merge_gateway::GOOGLE_GEMINI_3_7_FLASH.to_string(),
    );
    let request = LLMRequest {
        model: models::merge_gateway::GOOGLE_GEMINI_3_7_FLASH.to_string(),
        tools: Some(Arc::new(vec![
            ToolDefinition::function(
                "write_stdin".to_string(),
                "Write to a running command".to_string(),
                write_stdin_parameters(),
            ),
            ToolDefinition::function("apply_patch".to_string(), "Edit files".to_string(), apply_patch_parameters()),
        ])),
        ..Default::default()
    };

    let payload = provider.build_native_payload(&request, false).expect("payload");
    let tools = payload["tools"].as_array().expect("native tools");

    assert_eq!(tools.len(), 2);
    assert!(tools.iter().all(|tool| tool["parameters"].get("anyOf").is_none()));
    assert_eq!(tools[0]["parameters"]["required"], json!(["session_id"]));
    assert!(tools[1]["parameters"].get("required").is_none());
}

#[tokio::test]
async fn native_generate_uses_responses_endpoint_and_parses_tool_use_response() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "resp_123",
            "object": "response",
            "created_at": "2026-03-23T12:03:00Z",
            "model": "openai/gpt-5.1",
            "output": [{
                "type": "message",
                "id": "msg_1",
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Hello"},
                    {"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"location": "Paris"}}
                ],
                "finish_reason": "tool_use"
            }],
            "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "cost": 0.01}
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider
        .generate(LLMRequest {
            model: models::merge_gateway::DEFAULT_ROUTING.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("native generate");

    assert_eq!(response.content.as_deref(), Some("Hello"));
    assert_eq!(response.finish_reason, FinishReason::ToolCalls);
    assert_eq!(response.tool_calls.as_ref().expect("tool calls").len(), 1);
    let tool_call = &response.tool_calls.as_ref().expect("tool calls")[0];
    assert_eq!(tool_call.id, "call_1");
    assert_eq!(tool_call.tool_name(), Some("get_weather"));
    assert_eq!(tool_call.parsed_arguments().expect("tool args"), json!({"location": "Paris"}));
    assert_eq!(response.usage.as_ref().expect("usage").prompt_tokens, 10);
    assert_eq!(response.usage.as_ref().expect("usage").completion_tokens, 5);
    assert_eq!(response.usage.as_ref().expect("usage").total_tokens, 15);
    assert_eq!(response.request_id.as_deref(), Some("resp_123"));
}

#[tokio::test]
async fn response_delta_events_are_normalized_for_compatibility() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let stream_body = [
        sse("response.output_text.delta", json!({"delta": "Hello"})),
        sse(
            "response.output_item.added",
            json!({
                "output_item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "call_1", "name": "get_weather"}
                    ],
                    "finish_reason": "tool_use"
                }
            }),
        ),
        sse(
            "response.function_call_arguments.delta",
            json!({"call_id": "call_1", "delta": r#"{"location":"San Francisco"}"#}),
        ),
        sse(
            "response.output_item.done",
            json!({
                "output_item": {
                    "type": "message",
                    "id": "msg_1",
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"location": "San Francisco"}}
                    ],
                    "finish_reason": "tool_use"
                }
            }),
        ),
        sse(
            "response.completed",
            json!({
                "response": {
                    "id": "resp_123",
                    "object": "response",
                    "created_at": "2026-03-23T12:03:00Z",
                    "model": "openai/gpt-5.1",
                    "output": [{
                        "type": "message",
                        "id": "msg_1",
                        "role": "assistant",
                        "content": [
                            {"type": "text", "text": "Hello"},
                            {"type": "tool_use", "id": "call_1", "name": "get_weather", "input": {"location": "San Francisco"}}
                        ],
                        "finish_reason": "tool_use"
                    }],
                    "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "cost": 0.01}
                }
            }),
        ),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::DEFAULT_ROUTING.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::TextDelta { delta },
            NormalizedStreamEvent::ToolCallStart { call_id, name },
            NormalizedStreamEvent::ToolCallDelta { call_id: delta_call_id, delta: tool_delta },
            NormalizedStreamEvent::Usage { usage },
            NormalizedStreamEvent::Done { response }
        ]
        if delta == "Hello"
            && call_id == "call_1"
            && name.as_deref() == Some("get_weather")
            && delta_call_id == "call_1"
            && tool_delta == r#"{"location":"San Francisco"}"#
            && usage.prompt_tokens == 10
            && usage.completion_tokens == 5
            && usage.total_tokens == 15
            && response.content.as_deref() == Some("Hello")
            && response.tool_calls.as_ref().is_some_and(|calls| calls.len() == 1)
    ));
}

#[tokio::test]
async fn native_stream_snapshots_emit_tool_arguments_and_done_terminal() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let stream_response = json!({
        "id": "resp_123",
        "object": "response",
        "created_at": "2026-03-23T12:03:00Z",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "exec_command"
            }],
            "finish_reason": "tool_use"
        }],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "cost": 0.01}
    });
    let done_response = json!({
        "id": "resp_123",
        "object": "response",
        "created_at": "2026-03-23T12:03:00Z",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "id": "msg_1",
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "call_1",
                "name": "exec_command",
                "input": {"cmd": "pwd"}
            }],
            "finish_reason": "tool_use"
        }],
        "usage": {"input_tokens": 10, "output_tokens": 5, "total_tokens": 15, "cost": 0.01}
    });
    let stream_body = [
        sse("response.stream", stream_response),
        sse("response.done", done_response),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("run pwd".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::ToolCallStart { call_id, name },
            NormalizedStreamEvent::ToolCallDelta { call_id: delta_call_id, delta },
            NormalizedStreamEvent::Usage { usage },
            NormalizedStreamEvent::Done { response }
        ]
        if call_id == "call_1"
            && name.as_deref() == Some("exec_command")
            && delta_call_id == "call_1"
            && delta == r#"{"cmd":"pwd"}"#
            && usage.prompt_tokens == 10
            && usage.completion_tokens == 5
            && usage.total_tokens == 15
            && response.tool_calls.as_ref().is_some_and(|calls| {
                calls.len() == 1
                    && calls[0].tool_name() == Some("exec_command")
                    && calls[0].parsed_arguments().expect("tool args") == json!({"cmd": "pwd"})
            })
    ));
}

#[tokio::test]
async fn native_stream_snapshots_emit_text_before_terminal_frame() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let first_response = json!({
        "id": "resp_123",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello"}],
            "finish_reason": null
        }]
    });
    let cumulative_response = json!({
        "id": "resp_123",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "Hello world"}],
            "finish_reason": "stop"
        }]
    });
    let stream_body = [
        sse("response.stream", first_response),
        sse("response.stream", cumulative_response.clone()),
        sse("response.done", cumulative_response),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::TextDelta { delta: first_delta },
            NormalizedStreamEvent::TextDelta { delta: second_delta },
            NormalizedStreamEvent::Done { response }
        ]
        if first_delta == "Hello"
            && second_delta == " world"
            && response.content.as_deref() == Some("Hello world")
    ));
}

#[tokio::test]
async fn native_stream_uses_latest_cumulative_snapshot() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let first_response = json!({
        "id": "resp_123",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "obsolete"}],
            "finish_reason": "stop"
        }]
    });
    let final_response = json!({
        "id": "resp_123",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "final"}],
            "finish_reason": "stop"
        }]
    });
    let stream_body = [
        sse("response.stream", first_response),
        sse("response.stream", final_response.clone()),
        sse("response.done", final_response),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::TextDelta { delta },
            NormalizedStreamEvent::Done { response }
        ]
        if delta == "final" && response.content.as_deref() == Some("final")
    ));
}

#[tokio::test]
async fn native_stream_uses_object_field_for_frame_kind() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let first_response = json!({
        "id": "resp_123",
        "object": "response.stream",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "first"}],
            "finish_reason": null
        }]
    });
    let final_response = json!({
        "id": "resp_123",
        "object": "response.done",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "complete response"}],
            "finish_reason": "stop"
        }],
        "usage": {"input_tokens": 10, "output_tokens": 3, "total_tokens": 13}
    });
    let stream_body = [sse_data(first_response), sse_data(final_response)].concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::TextDelta { delta },
            NormalizedStreamEvent::Usage { usage },
            NormalizedStreamEvent::Done { response }
        ]
        if delta == "complete response"
            && usage.prompt_tokens == 10
            && usage.completion_tokens == 3
            && response.content.as_deref() == Some("complete response")
    ));
}

#[tokio::test]
async fn native_stream_prefers_sse_event_name_over_payload_kind() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let first_response = json!({
        "id": "resp_123",
        "type": "response",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "complete response"}],
            "finish_reason": null
        }]
    });
    let final_response = json!({
        "id": "resp_123",
        "type": "response",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "complete response"}],
            "finish_reason": "stop"
        }],
        "usage": {"input_tokens": 10, "output_tokens": 3, "total_tokens": 13}
    });
    let stream_body = [
        sse("response.stream", first_response),
        sse("response.done", final_response),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    assert!(matches!(
        events.as_slice(),
        [
            NormalizedStreamEvent::TextDelta { delta },
            NormalizedStreamEvent::Usage { usage },
            NormalizedStreamEvent::Done { response }
        ]
        if delta == "complete response"
            && usage.prompt_tokens == 10
            && usage.completion_tokens == 3
            && response.content.as_deref() == Some("complete response")
    ));
}

#[tokio::test]
async fn native_stream_fallback_restart_discards_previous_snapshot() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    let old_response = json!({
        "id": "resp_old",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [
                {"type": "text", "text": "old"},
                {"type": "tool_use", "id": "call_old", "name": "exec_command"}
            ],
            "finish_reason": "tool_use"
        }]
    });
    let new_response = json!({
        "id": "resp_new",
        "object": "response",
        "model": "xai/grok-4.6",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{"type": "text", "text": "new"}],
            "finish_reason": "stop"
        }],
        "usage": {"input_tokens": 3, "output_tokens": 1, "total_tokens": 4}
    });
    let stream_body = [
        sse("response.stream", old_response),
        sse_data(json!({"fallback_restart": true, "model": "xai/grok-4.6", "vendor": "xai"})),
        sse("response.stream", new_response.clone()),
        sse("response.done", new_response),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event.expect("stream event"));
    }

    let text = events
        .iter()
        .filter_map(|event| match event {
            NormalizedStreamEvent::TextDelta { delta } => Some(delta.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(text, "new", "events: {events:?}");
    assert!(matches!(
        events.last(),
        Some(NormalizedStreamEvent::Done { response })
            if response.content.as_deref() == Some("new")
                && response.request_id.as_deref() == Some("resp_new")
    ));
}

#[tokio::test]
async fn native_stream_error_frame_is_returned_as_provider_error() {
    let server = MockServer::start().await;
    let provider = test_provider(&server.uri());

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(sse(
                    "response.error",
                    json!({
                        "error": {
                            "type": "provider_error",
                            "message": "upstream failed",
                            "status_code": 502
                        }
                    }),
                )),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream_normalized(LLMRequest {
            model: models::merge_gateway::XAI_GROK_4_6.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("normalized stream");

    let error = stream.next().await.expect("error event").expect_err("stream should fail");
    assert!(error.to_string().contains("upstream failed"));
}

#[tokio::test]
async fn legacy_openai_base_url_uses_chat_completions_endpoint() {
    let server = MockServer::start().await;
    let provider = MergeGatewayProvider::from_config(
        Some("test-key".to_string()),
        Some(models::merge_gateway::DEFAULT_ROUTING.to_string()),
        Some(format!("{}/v1/openai", server.uri())),
        None,
        None,
        None,
        None,
    );

    Mock::given(method("POST"))
        .and(path("/v1/openai/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "chatcmpl_1",
            "choices": [{
                "finish_reason": "stop",
                "message": {"content": "legacy hello"}
            }]
        })))
        .expect(1)
        .mount(&server)
        .await;

    let response = provider
        .generate(LLMRequest {
            model: models::merge_gateway::DEFAULT_ROUTING.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("legacy generate");

    assert_eq!(response.content.as_deref(), Some("legacy hello"));
    assert_eq!(response.finish_reason, FinishReason::Stop);
}

/// A reported reasoning count must survive native usage parsing.
///
/// Both the native response and the native stream build their usage
/// through `parse_native_usage`, so this pins the shared assignment rather
/// than one caller's copy of it.
#[test]
fn native_usage_keeps_reported_reasoning_tokens() {
    let usage = MergeGatewayProvider::parse_native_usage(Some(&json!({
        "input_tokens": 10,
        "output_tokens": 5,
        "total_tokens": 15,
        "output_tokens_details": {"reasoning_tokens": 4}
    })))
    .expect("usage should parse");

    assert_eq!(usage.prompt_tokens, 10);
    assert_eq!(usage.completion_tokens, 5);
    assert_eq!(usage.reasoning_output_tokens, Some(4));
}
