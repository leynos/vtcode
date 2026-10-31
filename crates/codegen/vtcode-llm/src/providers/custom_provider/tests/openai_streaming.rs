//! Tests OpenAI-compatible custom-profile serialization and streamed tool calls.
use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message, ToolCall, ToolDefinition};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig,
};
use vtcode_config::types::ReasoningEffortLevel;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn openai_chat_custom_profile_preserves_reasoning_and_serialises_controls() {
    let server = MockServer::start().await;
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    let captured_for_mock = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            *captured_for_mock.lock().expect("capture mutex") = serde_json::from_slice(&request.body).ok();
            ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{
                    "message": {
                        "content": "answer",
                        "reasoning_content": "think first"
                    },
                    "finish_reason": "stop"
                }]
            }))
        })
        .mount(&server)
        .await;

    let model = "DeepSeek-V4-Flash-0731".to_owned();
    let mut config = CustomProviderConfig {
        name: "deepseek-custom".to_owned(),
        display_name: "DeepSeek Custom".to_owned(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: model.clone(),
        models: vec![model.clone()],
        ..Default::default()
    };
    config.profiles.insert(
        model.clone(),
        CustomProviderProfileConfig {
            supports_reasoning: Some(true),
            supports_reasoning_effort: Some(true),
            ..Default::default()
        },
    );

    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("test-key".to_owned()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let response = provider
        .generate(LLMRequest {
            model,
            messages: vec![Message::user("hello".to_owned())].into(),
            reasoning_effort: Some(ReasoningEffortLevel::High),
            ..Default::default()
        })
        .await
        .expect("custom OpenAI chat response should parse");

    assert_eq!(response.reasoning.as_deref(), Some("think first"));
    let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
    assert_eq!(payload["include_reasoning"], true);
    assert_eq!(payload["reasoning_effort"], "high");
}

async fn mount_fragmented_tool_call_stream(server: &MockServer, captured: Arc<Mutex<Option<Value>>>) {
    let captured_for_mock = Arc::clone(&captured);
    let body = concat!(
        "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Checking.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            *captured_for_mock.lock().expect("capture mutex") = serde_json::from_slice(&request.body).ok();
            ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
        })
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn openai_chat_stream_reassembles_fragmented_tool_call_playback() {
    let server = MockServer::start().await;
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
    mount_fragmented_tool_call_stream(&server, Arc::clone(&captured)).await;

    let model = "fixture-model".to_owned();
    let config = CustomProviderConfig {
        name: "streaming-custom".to_owned(),
        display_name: "Streaming Custom".to_owned(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: model.clone(),
        models: vec![model.clone()],
        supports_tools: Some(true),
        ..Default::default()
    };
    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("test-key".to_owned()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let tools = vec![ToolDefinition::function(
        "list_files".to_owned(),
        "List workspace files".to_owned(),
        json!({"type": "object", "properties": {"path": {"type": "string"}}}),
    )];
    let mut stream = provider
        .stream(LLMRequest {
            model,
            messages: vec![Message::user("inspect the workspace".to_owned())].into(),
            stream: true,
            tools: Some(Arc::new(tools)),
            ..Default::default()
        })
        .await
        .expect("custom OpenAI chat stream should start");

    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let LLMStreamEvent::Completed { response } = event.expect("stream event should decode") {
            completed = Some(*response);
        }
    }

    let response = completed.expect("stream should complete");
    assert_eq!(response.content.as_deref(), Some("Checking."));
    assert_eq!(
        response.tool_calls,
        Some(vec![ToolCall::function(
            "call_1".to_owned(),
            "list_files".to_owned(),
            r#"{"path":""}"#.to_owned(),
        )]),
    );
    let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
    assert_eq!(payload["stream"], true);
    assert_eq!(payload["tools"][0]["function"]["name"], "list_files");
}

#[tokio::test]
async fn openai_chat_stream_rejects_eof_without_a_terminal_frame() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "data: {\"id\":\"chatcmpl-truncated\",\"choices\":[{\"index\":0,",
                "\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
            ),
            "text/event-stream",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let model = "fixture-model".to_owned();
    let config = CustomProviderConfig {
        name: "truncated-custom".to_owned(),
        display_name: "Truncated Custom".to_owned(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        model: model.clone(),
        models: vec![model.clone()],
        ..Default::default()
    };
    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("test-key".to_owned()),
        Some(model.clone()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let mut stream = provider
        .stream(LLMRequest {
            model,
            messages: vec![Message::user("start then disconnect".to_owned())].into(),
            stream: true,
            ..Default::default()
        })
        .await
        .expect("truncated response should establish its stream");

    assert!(matches!(
        stream.next().await,
        Some(Ok(LLMStreamEvent::Token { delta })) if delta == "partial"
    ));
    assert!(matches!(stream.next().await, Some(Err(_))), "truncated stream should fail");
    assert!(stream.next().await.is_none(), "failed stream should then terminate");
}
