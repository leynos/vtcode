//! Regression tests for the Hugging Face provider.

use super::HuggingFaceProvider;
use crate::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message, ToolDefinition};
use crate::providers::common::{is_minimax_m2_model, normalize_reasoning_detail_object};
use futures::StreamExt;
use serde_json::json;
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[test]
fn minimax_model_detection_handles_variants() {
    assert!(is_minimax_m2_model("MiniMaxAI/MiniMax-M2.5:novita"));
    assert!(is_minimax_m2_model("minimax-m2.5"));
    assert!(!is_minimax_m2_model("deepseek-r1"));
}

#[test]
fn normalize_reasoning_detail_decodes_stringified_object() {
    let parsed = normalize_reasoning_detail_object(&json!("{\"type\":\"reasoning.text\",\"text\":\"step\"}"))
        .expect("expected a parsed reasoning detail object");
    assert!(parsed.is_object());
    assert_eq!(parsed["type"], "reasoning.text");
}

#[test]
fn serialize_messages_normalizes_minimax_reasoning_details() {
    let provider = HuggingFaceProvider::with_model("test-key".to_string(), "MiniMaxAI/MiniMax-M2.5:novita".to_string());
    let request = LLMRequest {
        model: "MiniMaxAI/MiniMax-M2.5:novita".to_string(),
        messages: vec![
            Message::assistant("answer".to_string())
                .with_reasoning_details(Some(vec![json!("{\"type\":\"reasoning.text\",\"text\":\"chain\"}")])),
        ]
        .into(),
        ..Default::default()
    };

    let messages = provider
        .serialize_messages_huggingface_chat(&request)
        .expect("message serialization should succeed");
    assert!(messages[0]["reasoning_details"].is_array());
    assert!(messages[0]["reasoning_details"][0].is_object());
}

#[test]
fn serialize_messages_rehydrates_glm_interleaved_history_into_content() {
    let provider = HuggingFaceProvider::with_model("test-key".to_string(), "zai-org/GLM-5.1:novita".into());
    let request = LLMRequest {
        model: "zai-org/GLM-5.1:novita".to_string(),
        messages: vec![Message::assistant("done".to_string()).with_reasoning(Some("trace".to_string()))].into(),
        ..Default::default()
    };

    let messages = provider
        .serialize_messages_huggingface_chat(&request)
        .expect("message serialization should succeed");

    assert_eq!(messages[0]["content"], json!("<think>trace</think>done"));
}

#[test]
fn normalize_step35_flash_provider_suffix() {
    let provider = HuggingFaceProvider::with_model("test-key".to_string(), "stepfun-ai/Step-3.5-Flash".to_string());

    let normalized = provider
        .normalize_model_id("stepfun-ai/Step-3.5-Flash")
        .expect("normalization should succeed");
    assert_eq!(normalized, "stepfun-ai/Step-3.5-Flash:featherless-ai".to_string());

    let normalized_legacy = provider
        .normalize_model_id("stepfun-ai/Step-3.5-Flash:fastest")
        .expect("legacy suffix normalization should succeed");
    assert_eq!(normalized_legacy, "stepfun-ai/Step-3.5-Flash:featherless-ai".to_string());
}

#[test]
fn format_for_chat_completions_keeps_apply_patch_as_function_tool() {
    let provider =
        HuggingFaceProvider::with_model("test-key".to_string(), "Qwen/Qwen3-Coder-480B-A35B-Instruct".to_string());
    let request = LLMRequest {
        model: "Qwen/Qwen3-Coder-480B-A35B-Instruct".to_string(),
        messages: vec![Message::user("apply a patch".to_string())].into(),
        tools: Some(Arc::new(vec![ToolDefinition::apply_patch("Apply patches".to_string())])),
        ..Default::default()
    };

    let payload = provider
        .format_for_chat_completions(&request)
        .expect("payload should serialize");

    assert_eq!(payload["tools"][0]["type"], "function");
    assert_eq!(payload["tools"][0]["function"]["name"], "apply_patch");
}

/// A reported reasoning count must survive the Responses-format response
/// parser.
#[test]
fn responses_api_usage_keeps_reported_reasoning_tokens() {
    let payload = json!({
        "output": [{
            "type": "message",
            "content": [{"type": "output_text", "text": "hi"}]
        }],
        "usage": {
            "input_tokens": 8,
            "output_tokens": 6,
            "total_tokens": 14,
            "output_tokens_details": {"reasoning_tokens": 2}
        }
    });

    let response = HuggingFaceProvider::parse_responses_api_format(&payload, "model-a".to_string())
        .expect("responses format should parse");
    let usage = response.usage.expect("usage should be present");
    assert_eq!(usage.reasoning_output_tokens, Some(2));
}

/// A reported reasoning count must survive the chat-completions stream.
#[tokio::test]
async fn chat_stream_keeps_reported_reasoning_tokens() {
    let server = MockServer::start().await;
    let provider = HuggingFaceProvider::from_config(
        Some("test-key".to_string()),
        Some("deepseek-ai/DeepSeek-V3".to_string()),
        Some(server.uri()),
        None,
        None,
        None,
        None,
    );

    let body = [
        format!("data: {}\n\n", json!({"choices": [{"delta": {"content": "hi"}}]})),
        format!(
            "data: {}\n\n",
            json!({
                "choices": [{"delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": 9,
                    "completion_tokens": 6,
                    "total_tokens": 15,
                    "completion_tokens_details": {"reasoning_tokens": 4}
                }
            })
        ),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream(LLMRequest {
            model: "deepseek-ai/DeepSeek-V3".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("stream should start");

    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let LLMStreamEvent::Completed { response } = event.expect("stream event") {
            completed = Some(response);
        }
    }

    let usage = completed.expect("a completed event").usage.expect("completed usage");
    assert_eq!(usage.reasoning_output_tokens, Some(4));
}

/// Usage may arrive after the frame that carries the finish reason.
#[tokio::test]
async fn chat_stream_captures_usage_from_a_frame_after_the_finish_reason() {
    let server = MockServer::start().await;
    let provider = HuggingFaceProvider::from_config(
        Some("test-key".to_string()),
        Some("deepseek-ai/DeepSeek-V3".to_string()),
        Some(server.uri()),
        None,
        None,
        None,
        None,
    );

    let body = [
        format!("data: {}\n\n", json!({"choices": [{"delta": {"content": "hi"}}]})),
        format!("data: {}\n\n", json!({"choices": [{"delta": {}, "finish_reason": "stop"}]})),
        format!(
            "data: {}\n\n",
            json!({
                "choices": [],
                "usage": {
                    "prompt_tokens": 9,
                    "completion_tokens": 6,
                    "total_tokens": 15,
                    "completion_tokens_details": {"reasoning_tokens": 4}
                }
            })
        ),
        "data: [DONE]\n\n".to_string(),
    ]
    .concat();

    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body),
        )
        .expect(1)
        .mount(&server)
        .await;

    let mut stream = provider
        .stream(LLMRequest {
            model: "deepseek-ai/DeepSeek-V3".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("stream should start");

    let mut completed = None;
    while let Some(event) = stream.next().await {
        if let LLMStreamEvent::Completed { response } = event.expect("stream event") {
            completed = Some(response);
        }
    }

    let usage = completed.expect("a completed event").usage.expect("completed usage");
    assert_eq!(usage.completion_tokens, 6);
    assert_eq!(usage.reasoning_output_tokens, Some(4));
}
