//! Explicit and automatic OpenAI request-format routing tests.

use super::*;

fn chat_completion_response_body(text: &str) -> Value {
    json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "created": 0,
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2 }
    })
}

fn responses_api_response_body(text: &str) -> Value {
    json!({
        "id": "resp_test",
        "object": "response",
        "model": "test-model",
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": text
            }]
        }],
        "usage": { "input_tokens": 1, "output_tokens": 1 }
    })
}

#[tokio::test]
async fn explicit_openai_chat_format_keeps_chat_completions_path() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(chat_completion_response_body("chat override")))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6.to_string()]),
    )
    .with_api_format_override(Some(CustomProviderApiFormat::OpenAIChat));

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6))
        .await
        .expect("chat override should succeed");

    assert_eq!(response.content.as_deref(), Some("chat override"));
}

#[tokio::test]
async fn explicit_openai_responses_format_keeps_responses_path() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_json(responses_api_response_body("responses override")))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6.to_string()]),
    )
    .with_api_format_override(Some(CustomProviderApiFormat::OpenAIResponses));

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6))
        .await
        .expect("responses override should succeed");

    assert_eq!(response.content.as_deref(), Some("responses override"));
}

#[tokio::test]
async fn auto_openai_format_retains_model_aware_responses_behaviour() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"auto responses\"}\n\n\
             data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_auto\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}\n\n",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "custom".to_string(),
        "Custom".to_string(),
        Some("test-key".to_string()),
        Some(models::openai::GPT_5_6_SOL.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        None,
        Some(vec![models::openai::GPT_5_6_SOL.to_string()]),
    );

    let response = provider
        .generate(sample_request(models::openai::GPT_5_6_SOL))
        .await
        .expect("auto behaviour should still use responses path");

    assert_eq!(response.content.as_deref(), Some("auto responses"));
}
