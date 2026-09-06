//! Chat Completions payload construction tests.

use super::*;

// ─── Chat Completions Payload Tests ──────────────────────────────────────────

#[test]
fn chat_completions_payload_uses_function_wrapper() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    let tools = payload.get("tools").and_then(Value::as_array).expect("tools should exist");
    let tool = tools[0].as_object().expect("tool entry should be object");
    assert!(tool.contains_key("function"));
    assert_eq!(tool.get("name").and_then(Value::as_str), Some("search_workspace"));
}

#[test]
fn chat_completions_applies_gpt_5_6_addendum_only_for_gpt_5_6() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    let system_content = messages
        .first()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("system content should be a string");
    assert!(system_content.contains("You are a helpful assistant."));
    assert!(system_content.contains("## GPT-5.6 OpenAI Addendum"));

    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.system_prompt = Some(Arc::from("You are a helpful assistant."));
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    let system_content = messages
        .first()
        .and_then(|message| message.get("content"))
        .and_then(Value::as_str)
        .expect("system content should be a string");
    assert!(!system_content.contains("## GPT-5.6 OpenAI Addendum"));
}

#[test]
fn chat_completions_uses_max_completion_tokens_field() {
    let provider = compatible_endpoint_provider(models::openai::DEFAULT_MODEL, "https://api.openai.com/v1");
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.max_tokens = Some(512);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert_eq!(payload.get("max_completion_tokens").and_then(Value::as_u64), Some(512));
    assert!(payload.get("max_tokens").is_none());
}

#[test]
fn custom_openai_provider_uses_compatible_max_tokens_field_even_on_openai_url() {
    let provider = OpenAIProvider::from_custom_config(
        "openai-compatible".to_string(),
        "OpenAI-compatible".to_string(),
        Some(String::new()),
        Some(models::openai::DEFAULT_MODEL.to_string()),
        Some("https://api.openai.com/v1".to_string()),
        None,
        None,
        None,
        None,
        None,
        None,
    );
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.max_tokens = Some(512);

    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");

    assert_eq!(payload.get("max_tokens").and_then(Value::as_u64), Some(512));
    assert!(payload.get("max_completion_tokens").is_none());
}

#[test]
fn chat_completions_applies_temperature_independent_of_max_tokens() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert!(payload.get("max_completion_tokens").is_none());
    let temp = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .expect("temperature should be present");
    assert!((temp - 0.4).abs() < 1e-6);
}

#[test]
fn chat_completions_omits_temperature_for_gpt_5_5_with_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::Medium);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert!(payload.get("temperature").is_none());
}

#[test]
fn chat_completions_keeps_temperature_for_gpt_5_5_without_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::None);
    request.temperature = Some(0.4);
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let temp = payload
        .get("temperature")
        .and_then(Value::as_f64)
        .expect("temperature should be present");
    assert!((temp - 0.4).abs() < 1e-6);
}

#[test]
fn chat_payload_omits_assistant_phase_metadata() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()),
            provider::Message::assistant("Working".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
        ]
        .into(),
        model: models::openai::DEFAULT_MODEL.to_string(),
        ..Default::default()
    };
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    let messages = payload
        .get("messages")
        .and_then(Value::as_array)
        .expect("messages should exist");
    assert!(messages[1].get("phase").is_none());
}

#[test]
fn chat_payload_rejects_file_url_content_parts() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let request = provider::LLMRequest {
        messages: vec![provider::Message::user_with_parts(vec![
            provider::ContentPart::file_from_url("https://example.com/doc.pdf".to_string()),
        ])]
        .into(),
        model: models::openai::DEFAULT_MODEL.to_string(),
        ..Default::default()
    };
    let err = provider
        .convert_to_openai_format(&request)
        .expect_err("chat payload should reject file_url");
    match err {
        provider::LLMError::InvalidRequest { message, .. } => {
            assert!(message.contains("does not support file_url"));
        }
        other => panic!("unexpected error variant: {other:?}"),
    }
}

#[test]
fn chat_payload_includes_prompt_cache_key_for_native_openai() {
    let provider = native_openai_provider(models::openai::DEFAULT_MODEL);
    let mut request = sample_request(models::openai::DEFAULT_MODEL);
    request.prompt_cache_key = Some("vtcode:openai:session-abc".to_string());
    let payload = provider.convert_to_openai_format(&request).expect("conversion should succeed");
    assert_eq!(payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:openai:session-abc"));
}

#[test]
fn chat_payload_uses_provider_level_service_tier_for_native_openai() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("priority"));
}

#[test]
fn chat_payload_uses_flex_service_tier_for_native_openai() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        None,
        None,
        None,
        None,
        Some(flex_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert_eq!(payload.get("service_tier").and_then(Value::as_str), Some("flex"));
}

#[test]
fn chat_payload_omits_service_tier_for_models_without_service_tier_support() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_OSS_20B.to_string()),
        None,
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::GPT_OSS_20B, &provider);
    assert!(payload.get("service_tier").is_none());
}

#[test]
fn chat_payload_omits_service_tier_for_non_native_openai_base_url() {
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::DEFAULT_MODEL.to_string()),
        Some("https://example.local/v1".to_string()),
        None,
        None,
        None,
        Some(priority_openai_config()),
        None,
    );
    let payload = chat_payload_for(models::openai::DEFAULT_MODEL, &provider);
    assert!(payload.get("service_tier").is_none());
}
