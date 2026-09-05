//! Responses request compatibility through the actual custom-provider router.

use serde_json::json;
use vtcode_config::core::{AnthropicConfig, CustomProviderApiFormat, CustomProviderConfig};
use vtcode_llm::provider::{LLMProvider, LLMRequest, Message, ToolDefinition};
use vtcode_llm::providers::CustomProviderBackendRouter;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn custom_responses_router_omits_optional_format_and_preserves_raw_tool_history() {
    let server = MockServer::start().await;
    let input = "*** Begin Patch\n*** Update File: café.txt\n@@\n-old\n+quote: \"\\\"\n*** End Patch\n";
    Mock::given(method("POST"))
        .and(path("/responses"))
        .and(body_partial_json(json!({"model":"fixture-model"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id":"resp_fixture","status":"completed","model":"fixture-model",
            "output":[{"type":"custom_tool_call","id":"item_fixture","call_id":"call_fixture","name":"fixture_raw","input":input}],
            "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}
        })))
        .expect(2)
        .mount(&server).await;
    let config = CustomProviderConfig {
        name: "fixture-responses".into(),
        display_name: "Fixture Responses".into(),
        base_url: server.uri(),
        model: "fixture-model".into(),
        models: vec!["fixture-model".into()],
        api_format: CustomProviderApiFormat::OpenAIResponses,
        supports_tools: Some(true),
        ..Default::default()
    };
    let provider = CustomProviderBackendRouter::from_config(
        config,
        Some("synthetic-key".into()),
        Some("fixture-model".into()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        None,
    );
    let mut request = LLMRequest {
        model: "fixture-model".into(),
        max_tokens: Some(64),
        messages: vec![Message::user("Return a fixture patch".into())].into(),
        tools: Some(
            vec![ToolDefinition::custom(
                "fixture_raw".into(),
                "Synthetic raw input".into(),
            )]
            .into(),
        ),
        ..Default::default()
    };
    let response = provider.generate(request.clone()).await.unwrap();
    let calls = response.tool_calls.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].raw_input(), Some(input));
    assert_eq!(response.usage.unwrap().total_tokens, 0);
    request.messages = vec![
        Message::assistant_with_tools(String::new(), calls),
        Message::tool_response("call_fixture".into(), "fixture-only acknowledgement".into()),
        Message::user("Continue".into()),
    ]
    .into();
    let _response = provider.generate(request).await.unwrap();
    let requests = server.received_requests().await.unwrap();
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(first["tools"][0]["type"], "custom");
    assert!(first["tools"][0].get("format").is_none());
    let second: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(second["input"][0]["type"], "custom_tool_call");
    assert_eq!(second["input"][0]["input"], input);
    assert_eq!(second["input"][1]["type"], "custom_tool_call_output");
    assert_eq!(second["input"][1]["call_id"], "call_fixture");
    assert!(second.get("previous_response_id").is_none());
}
