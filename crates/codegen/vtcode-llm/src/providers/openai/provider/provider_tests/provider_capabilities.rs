//! Provider capability and response-phase behaviour tests.

use super::*;

// Phase behaviour: native OpenAI includes assistant phase, non-native omits it
#[test]
fn responses_payload_phase_behaviour() {
    // Native: includes phase for assistant, omits for user/tool
    let native = native_openai_provider(models::openai::GPT_5_6_SOL);
    let request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::assistant("Checking.".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::assistant_with_tools(
                "Looking up.".to_owned(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "search_workspace".to_string(),
                    r#"{"query":"x"}"#.to_string(),
                )],
            )
            .with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::tool_response("call_1".to_string(), "{\"ok\":true}".to_string())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
        ]
        .into(),
        model: models::openai::GPT_5_6_SOL.to_string(),
        ..Default::default()
    };
    let payload = native.convert_to_openai_responses_format(&request).expect("should succeed");
    let input = get_input_array(&payload);
    assert!(input[0].get("phase").is_none(), "user omits phase");
    assert_eq!(input[1].get("phase").and_then(Value::as_str), Some("commentary"));
    assert_eq!(input[2].get("phase").and_then(Value::as_str), Some("commentary"));
    assert!(input[3].get("phase").is_none(), "tool response omits phase");

    // Non-native: omits phase entirely
    let non_native = compatible_endpoint_provider(models::openai::GPT_5_6_SOL, "https://example.local/v1");
    let request2 = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Start".to_owned()),
            provider::Message::assistant("Checking.".to_owned()).with_phase(Some(provider::AssistantPhase::Commentary)),
        ]
        .into(),
        model: models::openai::GPT_5_6_SOL.to_string(),
        ..Default::default()
    };
    let payload2 = non_native
        .convert_to_openai_responses_format(&request2)
        .expect("should succeed");
    assert!(get_input_array(&payload2)[1].get("phase").is_none());
}

// ─── ChatGPT Backend Omissions ───────────────────────────────────────────────

#[test]
fn chatgpt_backend_forces_store_false_and_omits_output_sampling_cache() {
    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig { responses_store: Some(true), ..Default::default() }),
        None,
    );
    let payload = responses_payload_for(models::openai::GPT_5_6, &provider);
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "output_types");
    assert_absent(&payload, "sampling_parameters");
    assert_absent(&payload, "prompt_cache_retention");

    // With temperature/top_p set, still omitted
    let mut request = sample_request(models::openai::GPT_5_6);
    request.temperature = Some(0.4);
    request.top_p = Some(0.8);
    let payload2 = provider.convert_to_openai_responses_format(&request).expect("should succeed");
    assert_absent(&payload2, "sampling_parameters");
}

#[test]
fn chatgpt_backend_includes_encrypted_reasoning_and_preserves_configured_includes() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6_SOL);
    let payload = responses_payload_for(models::openai::GPT_5_6_SOL, &provider);
    assert_eq!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));

    let provider = OpenAIProvider::from_config(
        Some(String::new()),
        Some(sample_chatgpt_auth_handle()),
        Some(models::openai::GPT_5_6_SOL.to_string()),
        None,
        None,
        None,
        None,
        Some(OpenAIConfig {
            responses_include: vec![
                "output_text.annotations".to_string(),
                " reasoning.encrypted_content ".to_string(),
            ],
            ..Default::default()
        }),
        None,
    );
    let payload = responses_payload_for(models::openai::GPT_5_6_SOL, &provider);
    assert_eq!(
        payload.get("include").and_then(Value::as_array),
        Some(&vec![json!("output_text.annotations"), json!("reasoning.encrypted_content"),])
    );
}

#[test]
fn chatgpt_backend_disables_chat_completions_fallback() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    assert!(provider.is_chatgpt_backend());
    assert!(!provider.allows_chat_completions_fallback());
}

// ─── Compaction & Responses API State ────────────────────────────────────────

#[test]
fn supports_responses_compaction_tracks_responses_api_availability() {
    let openai = native_openai_provider(models::openai::GPT_5);
    assert!(openai.supports_responses_compaction(models::openai::GPT_5));
    let compatible = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    assert!(compatible.supports_responses_compaction(models::openai::GPT_5));
    let xai = compatible_endpoint_provider(models::openai::GPT_5, "https://api.x.ai/v1");
    assert!(!xai.supports_responses_compaction(models::openai::GPT_5));
}

#[test]
fn supports_manual_openai_compaction_is_native_only() {
    let openai = native_openai_provider(models::openai::GPT_5);
    assert!(openai.supports_manual_openai_compaction(models::openai::GPT_5));
    assert!(
        !compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1")
            .supports_manual_openai_compaction(models::openai::GPT_5)
    );
    assert!(
        !OpenAIProvider::from_custom_config(
            "custom".to_string(),
            "Custom".to_string(),
            Some(String::new()),
            Some(models::openai::GPT_5.to_string()),
            Some("https://api.openai.com/v1".to_string()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .supports_manual_openai_compaction(models::openai::GPT_5)
    );
    assert!(!chatgpt_backend_provider(models::openai::GPT_5).supports_manual_openai_compaction(models::openai::GPT_5));
    assert!(!openai.supports_manual_openai_compaction("gpt-4.1"));
}

#[test]
fn manual_openai_compaction_unavailable_message_mentions_backend() {
    let chatgpt = chatgpt_backend_provider(models::openai::GPT_5);
    let msg = chatgpt.manual_openai_compaction_unavailable_message(models::openai::GPT_5);
    assert!(msg.contains("ChatGPT subscription auth via chatgpt.com backend"));
    let compatible = compatible_endpoint_provider(models::openai::GPT_5, "https://compat.example/v1");
    let msg2 = compatible.manual_openai_compaction_unavailable_message(models::openai::GPT_5);
    assert!(msg2.contains("configured OpenAI-compatible endpoint (https://compat.example/v1)"));
    let openai = native_openai_provider(models::openai::GPT_5);
    let msg3 = openai.manual_openai_compaction_unavailable_message("gpt-4.1");
    assert!(msg3.contains("native OpenAI API (api.openai.com)"));
    assert!(msg3.contains("openai / native OpenAI API (api.openai.com) / gpt-4.1"));
}

// ─── Supported Models & Harmony ──────────────────────────────────────────────

#[test]
fn supported_models_include_current_reasoning_models() {
    let supported = OpenAIProvider::new("key".to_owned()).supported_models();
    // Current reasoning models must be in the supported list.
    assert!(supported.contains(&"gpt-5.6-sol".to_string()));
    assert!(supported.contains(&models::openai::GPT_6_ASTRA.to_string()));
    assert!(!supported.contains(&models::openai::GPT_5_CODEX.to_string()));
    // Deprecated o-series models are removed from the picker but retained in
    // REASONING_MODELS for backward-compat routing.
    assert!(!supported.contains(&models::openai::O3.to_string()));
    assert!(!supported.contains(&models::openai::O4_MINI.to_string()));
    assert!(models::openai::REASONING_MODELS.contains(&models::openai::O3));
    assert!(models::openai::REASONING_MODELS.contains(&models::openai::O4_MINI));
}

#[test]
fn harmony_detection_handles_common_variants() {
    assert!(OpenAIProvider::uses_harmony("gpt-oss-20b"));
    assert!(OpenAIProvider::uses_harmony("openai/gpt-oss-20b:free"));
    assert!(OpenAIProvider::uses_harmony("OPENAI/GPT-OSS-120B"));
    assert!(!OpenAIProvider::uses_harmony("gpt-5"));
    assert!(!OpenAIProvider::uses_harmony("gpt-oss:20b"));
}

// ─── Prompt Cache & Websocket ────────────────────────────────────────────────

use vtcode_config::core::PromptCachingConfig;
