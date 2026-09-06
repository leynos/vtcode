//! Prompt-cache, configuration, and sampling payload tests.

use super::*;

#[test]
fn responses_payload_includes_prompt_cache_retention_for_native_openai() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::H24);
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc),
        None,
        None,
        None,
        None,
    );
    // Responses API model
    let payload = responses_payload_for(models::openai::GPT_5_CODEX, &provider);
    assert_eq!(payload.get("prompt_cache_retention").and_then(Value::as_str), Some("24h"));
    // Chat Completions model - should NOT have it
    let chat_payload = chat_payload_for(models::openai::GPT_5, &provider);
    assert_absent(&chat_payload, "prompt_cache_retention");
}

#[test]
fn responses_payload_includes_prompt_cache_key_for_native_openai() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:openai:session-123".to_string());
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_eq!(payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:openai:session-123"));
}

#[test]
fn responses_payload_omits_prompt_cache_key_for_non_native() {
    let provider = compatible_endpoint_provider(models::openai::GPT_5_6, "https://example.local/v1");
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:openai:session-xyz".to_string());
    assert_absent(&provider.convert_to_openai_responses_format(&request).expect("should succeed"), "prompt_cache_key");
}

#[test]
fn prompt_cache_retention_excluded_when_not_set_and_for_unsupported_models() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = None;
    let provider = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc),
        None,
        None,
        None,
        None,
    );
    let mut request = sample_request(models::openai::GPT_5_6);
    request.stream = true;
    assert_absent(
        &provider.convert_to_openai_responses_format(&request).expect("should succeed"),
        "prompt_cache_retention",
    );

    // Unsupported model also omits it
    let mut pc2 = PromptCachingConfig::default();
    pc2.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::H24);
    let provider2 = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_OSS_20B.to_string()),
        None,
        Some(pc2),
        None,
        None,
        None,
        None,
    );
    assert_absent(&responses_payload_for(models::openai::GPT_OSS_20B, &provider2), "prompt_cache_retention");
}

#[test]
fn provider_from_config_respects_prompt_cache_and_websocket_gating() {
    let mut pc = PromptCachingConfig::default();
    pc.providers.openai.prompt_cache_retention = Some(PromptCacheRetention::InMemory);
    let provider = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        Some(pc.clone()),
        None,
        None,
        None,
        None,
    );
    assert_eq!(provider.prompt_cache_settings.prompt_cache_retention, Some(PromptCacheRetention::InMemory));

    let native_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(native_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let compatible_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://compat.example/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(compatible_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let custom_ws = OpenAIProvider::from_custom_config(
        "mycorp".to_string(),
        "MyCorp".to_string(),
        Some("key".to_string()),
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://compat.example/v1".to_string()),
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
        None,
        None,
    );
    assert!(custom_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let chatgpt_ws = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(!chatgpt_ws.websocket_mode_enabled(models::openai::GPT_5_6));

    let xai_ws = OpenAIProvider::from_config(
        Some("key".to_string()),
        None,
        Some(models::openai::GPT_5_6.to_string()),
        Some("https://api.x.ai/v1".to_string()),
        None,
        None,
        None,
        Some(OpenAIConfig { websocket_mode: true, ..Default::default() }),
        None,
    );
    assert!(!xai_ws.websocket_mode_enabled(models::openai::GPT_5_6));
}

// ─── Max Tokens & Reasoning ──────────────────────────────────────────────────

#[test]
fn responses_payload_uses_max_output_tokens_field() {
    let provider = native_openai_provider(models::openai::GPT_5);
    let mut request = sample_request(models::openai::GPT_5);
    request.max_tokens = Some(512);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_eq!(payload.get("max_output_tokens").and_then(Value::as_u64), Some(512));
    assert_absent(&payload, "max_completion_tokens");
}

#[test]
fn chatgpt_backend_omits_max_output_tokens_and_maps_minimal_reasoning() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = sample_request(models::openai::GPT_5_CODEX);
    request.max_tokens = Some(512);
    assert_absent(&provider.convert_to_openai_responses_format(&request).expect("should succeed"), "max_output_tokens");

    request.max_tokens = None;
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::Minimal);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_eq!(payload["reasoning"].get("effort").and_then(Value::as_str), Some("low"));
}

#[test]
fn responses_payload_defaults_gpt_5_4_reasoning_to_none() {
    let payload =
        responses_payload_for(models::openai::GPT_5_6_SOL, &native_openai_provider(models::openai::GPT_5_6_SOL));
    assert_eq!(payload.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str), Some("none"));
}

#[test]
fn responses_payload_defaults_gpt_5_3_codex_reasoning_to_high() {
    let payload =
        responses_payload_for(models::openai::GPT_5_CODEX, &native_openai_provider(models::openai::GPT_5_CODEX));
    assert_eq!(payload.get("reasoning").and_then(|r| r.get("effort")).and_then(Value::as_str), Some("high"));
}

#[test]
fn responses_payload_omits_sampling_parameters_for_gpt_5_4_high_reasoning() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::High);
    request.temperature = Some(0.4);
    request.top_p = Some(0.9);
    assert_absent(
        &provider.convert_to_openai_responses_format(&request).expect("should succeed"),
        "sampling_parameters",
    );
}

#[test]
fn responses_payload_omits_penalties_when_sampling_gate_closed() {
    let provider = native_openai_provider(models::openai::GPT_5_6_SOL);
    let mut request = sample_request(models::openai::GPT_5_6_SOL);
    request.reasoning_effort = Some(vtcode_config::types::ReasoningEffortLevel::High);
    request.presence_penalty = Some(0.1);
    request.frequency_penalty = Some(-0.5);
    let payload = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_absent(&payload, "sampling_parameters");
}
