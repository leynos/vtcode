use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message, ToolCall, ToolDefinition};
use futures::StreamExt;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig, CustomProviderProfileConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn collect_completed_response(
    provider: &CustomProviderBackendRouter,
    model: &str,
) -> crate::provider::LLMResponse {
    let mut stream = provider
        .stream(LLMRequest {
            model: model.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            stream: true,
            ..Default::default()
        })
        .await
        .expect("stream should start");

    while let Some(event) = stream.next().await {
        if let LLMStreamEvent::Completed { response } = event.expect("stream event should decode") {
            return *response;
        }
    }

    panic!("stream should yield a completed response");
}

#[tokio::test]
async fn openai_chat_stream_usage_obeys_profile_precedence_and_decodes_terminal_usage() {
    const OPTED_IN_MODEL: &str = "baseten/usage";
    const OPTED_OUT_MODEL: &str = "baseten/no-usage";

    let server = MockServer::start().await;
    let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured_for_mock = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(move |request: &wiremock::Request| {
            captured_for_mock
                .lock()
                .expect("capture mutex")
                .push(serde_json::from_slice(&request.body).expect("valid request JSON"));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(
                    "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n\
                     data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":5,\"total_tokens\":18,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n\
                     data: [DONE]\n\n",
                )
        })
        .expect(2)
        .mount(&server)
        .await;

    let config = CustomProviderConfig {
        name: "baseten".to_string(),
        display_name: "Baseten".to_string(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::OpenAIChat,
        supports_stream_usage: Some(true),
        model: OPTED_IN_MODEL.to_string(),
        models: vec![OPTED_IN_MODEL.to_string(), OPTED_OUT_MODEL.to_string()],
        profiles: std::collections::BTreeMap::from([(
            OPTED_OUT_MODEL.to_string(),
            CustomProviderProfileConfig {
                supports_stream_usage: Some(false),
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    let router = CustomProviderBackendRouter::from_config(
        config,
        Some("fixture-key".to_string()),
        Some(OPTED_IN_MODEL.to_string()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );

    let response = collect_completed_response(&router, OPTED_IN_MODEL).await;
    let usage = response.usage.expect("terminal Baseten usage should be retained");
    assert_eq!((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens), (13, 5, 18));
    assert_eq!(usage.reasoning_output_tokens, Some(3));
    drop(collect_completed_response(&router, OPTED_OUT_MODEL).await);

    let requests = captured.lock().expect("capture mutex");
    let opted_in = requests
        .iter()
        .find(|request| request["model"] == OPTED_IN_MODEL)
        .expect("opted-in request should be captured");
    let opted_out = requests
        .iter()
        .find(|request| request["model"] == OPTED_OUT_MODEL)
        .expect("opted-out request should be captured");
    assert_eq!(opted_in["stream_options"]["include_usage"], true);
    assert!(opted_out.get("stream_options").is_none());
}

#[tokio::test]
async fn openai_chat_stream_reassembles_fragmented_tool_call_playback() {
    let server = MockServer::start().await;
    let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
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
        .mount(&server)
        .await;

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
