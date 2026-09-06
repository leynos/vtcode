//! Request execution, retry, Harmony, and redaction tests.

use super::*;

// ─── Streaming Tests ─────────────────────────────────────────────────────────

#[test]
fn openai_models_support_streaming() {
    for model in [
        models::openai::GPT,
        models::openai::GPT_5,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_MINI,
        models::openai::GPT_5_NANO,
    ] {
        let provider = test_provider("http://test", model);
        assert!(provider.supports_streaming(), "Model {model} should support streaming");
    }
}

#[test]
fn native_stream_required_models_disable_non_streaming() {
    for model in [
        models::openai::GPT,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
        models::openai::GPT_5_6_SOL,
    ] {
        let provider = test_provider("http://test", model);
        assert!(!provider.supports_non_streaming(model), "Model {model} should require streaming");
    }
}

#[test]
fn chatgpt_backend_keeps_streaming_for_codex_and_disables_non_streaming() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    assert!(provider.supports_streaming());
    assert!(!provider.supports_non_streaming(models::openai::GPT_5_CODEX));
}

// ─── Harmony Parsing ─────────────────────────────────────────────────────────

#[test]
fn parse_harmony_tool_names_and_calls() {
    assert_eq!(
        OpenAIProvider::parse_harmony_tool_name("functions.code_search"),
        vtcode_config::constants::tools::CODE_SEARCH
    );
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("grep"), "grep");
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("container.exec"), "exec_command");
    assert_eq!(OpenAIProvider::parse_harmony_tool_name("unknown.tool"), "tool");
    assert!(!OpenAIProvider::uses_harmony("gpt-oss:20b"));

    let (name, args) = OpenAIProvider::parse_harmony_tool_call_from_text(
        r#"to=functions.code_search {"query":"Widget", "path":"src"}"#,
    )
    .expect("should parse");
    assert_eq!(name, vtcode_config::constants::tools::CODE_SEARCH);
    assert_eq!(args["query"], json!("Widget"));
    assert_eq!(args["path"], json!("src"));

    let (name2, args2) =
        OpenAIProvider::parse_harmony_tool_call_from_text(r#"to=container.exec {"cmd":["ls", "-la"]}"#)
            .expect("should parse");
    assert_eq!(name2, "exec_command");
    assert_eq!(args2["cmd"], json!(["ls", "-la"]));

    let text = r#"<|start|>assistant to=functions.lookup_weather<|channel|>commentary <|constrain|>json<|message|>{"location":"San Francisco"}<|call|>"#;
    let (name3, args3) = OpenAIProvider::parse_harmony_tool_call_from_text(text).expect("should parse");
    assert_eq!(name3, "lookup_weather");
    assert_eq!(args3["location"], json!("San Francisco"));

    let text2 = r#"<|start|>assistant to=functions.lookup_weather<|channel|>commentary <|constrain|>json<|message|>{'location':'San Francisco'}<|call|>"#;
    let (name4, _) = OpenAIProvider::parse_harmony_tool_call_from_text(text2).expect("should parse");
    assert_eq!(name4, "lookup_weather");
}

// ─── Retry & Fallback Tests ──────────────────────────────────────────────────

#[tokio::test]
async fn responses_request_retries_with_fallback_model_after_not_found() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5_NANO);
    let seen_models = Arc::new(Mutex::new(Vec::new()));
    let seen_for_mock = Arc::clone(&seen_models);

    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let payload: Value = serde_json::from_slice(&req.body).expect("valid json");
            let model = payload.get("model").and_then(Value::as_str).expect("model required");
            seen_for_mock.lock().expect("not poisoned").push(model.to_string());
            match model {
                models::openai::GPT_5_NANO => ResponseTemplate::new(404).set_body_string("model_not_found"),
                models::openai::GPT_5_MINI => ResponseTemplate::new(200).set_body_json(json!({
                    "id": "resp_fallback", "status": "completed",
                    "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"fallback response"}]}]
                })),
                other => ResponseTemplate::new(500).set_body_string(format!("unexpected: {other}")),
            }
        }).expect(2).mount(&server).await;

    let response = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_NANO.to_string(),
            ..Default::default()
        })
        .await
        .expect("fallback should succeed");
    assert_eq!(response.content.as_deref(), Some("fallback response"));
    assert_eq!(
        seen_models.lock().expect("not poisoned").as_slice(),
        &[
            models::openai::GPT_5_NANO.to_string(),
            models::openai::GPT_5_MINI.to_string()
        ]
    );
}

#[tokio::test]
async fn responses_request_retries_without_flex_service_tier() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = OpenAIProvider::from_config(
        Some("key".to_owned()),
        None,
        Some(models::openai::GPT_5_CODEX.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        Some(flex_openai_config()),
        None,
    );
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_mock = Arc::clone(&seen);

    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(mock_service_tier_fallback(seen_for_mock, json!({
            "id": "resp_retry", "status": "completed",
            "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"retry without flex succeeded"}]}]
        }))).expect(2).mount(&server).await;

    let response = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_CODEX.to_string(),
            ..Default::default()
        })
        .await
        .expect("retry without flex should succeed");
    assert_eq!(response.content.as_deref(), Some("retry without flex succeeded"));
    assert_eq!(seen.lock().expect("not poisoned").as_slice(), &[Some("flex".to_string()), None]);
}

// ─── Request Metadata & Content Type ─────────────────────────────────────────

#[tokio::test]
async fn responses_requests_include_client_request_id_and_debug_metadata() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&server.uri(), models::openai::GPT_5);
    Mock::given(method("POST")).and(path("/responses"))
        .respond_with(|req: &wiremock::Request| {
            let req_id = req.headers.get("x-client-request-id").and_then(|v| v.to_str().ok())
                .expect("x-client-request-id required");
            assert!(req_id.starts_with("vtcode-"));
            ResponseTemplate::new(400)
                .insert_header("x-request-id", "req_123")
                .insert_header("retry-after", "15")
                .set_body_string(r#"{"error":{"message":"Bad request","type":"invalid_request_error","param":"text.verbosity","code":"unsupported_parameter"}}"#)
        }).expect(1).mount(&server).await;
    let err = provider
        .generate(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5.to_string(),
            ..Default::default()
        })
        .await
        .expect_err("should surface error");
    let text = err.to_string();
    assert!(
        text.contains("request_id=req_123")
            && text.contains("client_request_id=vtcode-")
            && text.contains("retry_after=15")
            && text.contains("type=invalid_request_error")
    );
}

// ─── Manual Compaction ───────────────────────────────────────────────────────

#[tokio::test]
async fn manual_compaction_payload_includes_selected_fields_and_appends_instructions() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = test_provider(&native_openai_mock_base_url(&server), models::openai::GPT_5_6_SOL);
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    Mock::given(method("POST"))
        .and(path("/responses/compact"))
        .respond_with(move |req: &wiremock::Request| {
            *captured_for_mock.lock().expect("not poisoned") =
                Some(serde_json::from_slice(&req.body).expect("valid json"));
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "resp_compact", "status": "completed",
                "output": [{"type":"message","role":"assistant","content":[{"type":"output_text","text":"compacted"}]}]
            }))
        })
        .expect(1)
        .mount(&server)
        .await;

    let compacted = provider
        .compact_history_with_options(
            models::openai::GPT_5_6_SOL,
            &[
                provider::Message::system("Preserve decisions.".to_string()),
                provider::Message::user("Summarize.".to_string()),
            ],
            &provider::ResponsesCompactionOptions {
                instructions: Some("Terse.".to_string()),
                max_output_tokens: Some(321),
                reasoning_effort: Some(vtcode_config::types::ReasoningEffortLevel::Minimal),
                verbosity: Some(vtcode_config::types::VerbosityLevel::High),
                responses_include: Some(vec!["reasoning.encrypted_content".to_string()]),
                response_store: Some(true),
                service_tier: Some("priority".to_string()),
                prompt_cache_key: Some("lineage-key".to_string()),
            },
        )
        .await
        .expect("compaction should succeed");
    assert_eq!(compacted.len(), 1);

    let p = captured.lock().expect("not poisoned").clone().expect("payload captured");
    assert_eq!(p["model"], json!(models::openai::GPT_5_6_SOL));
    assert_eq!(p["max_output_tokens"], json!(321));
    assert_eq!(p["service_tier"], json!("priority"));
    assert_eq!(p["store"], json!(false));
    assert_eq!(p["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(p["reasoning"]["effort"], json!("minimal"));
    assert_eq!(p["text"]["verbosity"], json!("high"));
    assert_eq!(p["prompt_cache_key"], json!("lineage-key"));
    assert!(p.get("previous_response_id").is_none() && p.get("output_types").is_none() && p.get("stream").is_none());
    let instr = p["instructions"].as_str().expect("instructions required");
    assert!(
        instr.contains("Preserve decisions.")
            && instr.contains("[Manual Compaction Instructions]")
            && instr.contains("Terse.")
    );
}

// ─── Debug redaction for OpenAIRequestAuth ───────────────────────────────────

#[test]
fn openai_request_auth_debug_redacts_bearer_token() {
    use super::super::super::backend_setup::OpenAIRequestAuth;

    let auth = OpenAIRequestAuth::bearer_token("sk-secret-bearer-token".to_string());
    let debug_str = format!("{auth:?}");
    assert!(!debug_str.contains("sk-secret-bearer-token"), "bearer token leaked in Debug: {debug_str}");
}

#[test]
fn openai_request_auth_debug_redacts_rig_chatgpt_auth() {
    use super::super::super::backend_setup::OpenAIRequestAuth;

    let auth = OpenAIRequestAuth {
        bearer_token: "rig-secret-access".to_string(),
        chatgpt_account_id: Some("acc_123".to_string()),
        rig_chatgpt_auth: Some(RigChatGptAuth::AccessToken {
            access_token: "rig-secret-access".to_string(),
            account_id: Some("acc_123".to_string()),
        }),
    };
    let debug_str = format!("{auth:?}");
    assert!(!debug_str.contains("rig-secret-access"), "rig access token leaked in Debug: {debug_str}");
    // Non-secret metadata should still be visible.
    assert!(debug_str.contains("acc_123"), "account_id should be visible: {debug_str}");
}
