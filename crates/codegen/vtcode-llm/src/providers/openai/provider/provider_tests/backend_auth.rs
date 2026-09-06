//! Backend selection and authentication contract tests.

use super::*;

// ─── Auth & Retry Tests ──────────────────────────────────────────────────────

#[test]
fn openai_auth_backend_setup_uses_api_key_base_url_and_static_auth() {
    let setup = OpenAIBackendSetup::from_api_key_config(None);
    let transport = setup.transport();
    let responses = setup.responses_defaults();

    assert_eq!(setup.base_url(), urls::OPENAI_API_BASE);
    assert!(matches!(setup.kind(), OpenAIBackendKind::ApiKey));
    assert_eq!(setup.refresh_behaviour(), OpenAIBackendRefreshBehaviour::StaticBearer);
    assert!(transport.websocket);
    assert!(transport.chat_completions_fallback);
    assert!(transport.responses_compaction_endpoint);
    assert!(!responses.force_store_false);
    assert!(responses.include_prompt_cache_retention);
    assert!(!responses.include_encrypted_reasoning);
}

#[test]
fn chatgpt_auth_backend_setup_uses_rig_chatgpt_by_default() {
    let setup = OpenAIBackendSetup::from_chatgpt_subscription_config(None);
    let transport = setup.transport();
    let responses = setup.responses_defaults();

    assert_eq!(setup.base_url(), CHATGPT_CODEX_BASE);
    assert!(matches!(
        setup.kind(),
        OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::RigChatGpt)
    ));
    assert_eq!(setup.refresh_behaviour(), OpenAIBackendRefreshBehaviour::RefreshableChatGptSession);
    assert!(!transport.websocket);
    assert!(!transport.chat_completions_fallback);
    assert!(!transport.responses_compaction_endpoint);
    assert!(responses.force_store_false);
    assert!(!responses.include_prompt_cache_retention);
    assert!(responses.include_encrypted_reasoning);
    assert!(!responses.include_structured_history_in_input);
    assert!(responses.preserve_structured_history_on_replay);
}

#[test]
fn chatgpt_auth_backend_setup_keeps_compatibility_auth_behind_boundary() {
    let setup = OpenAIBackendSetup::chatgpt_subscription_compatibility(CHATGPT_CODEX_BASE.to_string());

    assert_eq!(
        setup.kind(),
        &OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::CodexAppServerCompatibility)
    );
    assert!(
        setup.uses_refreshable_auth(),
        "ChatGPT compatibility auth must remain refreshable behind the backend setup boundary"
    );

    let auth = setup.request_auth_from_session(OpenAIChatGptSession {
        openai_api_key: "exchanged-api-key".to_string(),
        id_token: "id-token".to_string(),
        access_token: "oauth-access".to_string(),
        refresh_token: "refresh-token".to_string(),
        account_id: Some("acc_123".to_string()),
        email: Some("test@example.com".to_string()),
        plan: Some("plus".to_string()),
        obtained_at: 1,
        refreshed_at: 1,
        expires_at: None,
    });
    assert_eq!(auth.bearer_token, "oauth-access");
    assert!(auth.rig_chatgpt_auth.is_none(), "compatibility fallback should not masquerade as Rig-backed auth");
}

#[test]
fn chatgpt_auth_backend_setup_applies_account_headers_without_session_id() {
    let setup = OpenAIBackendSetup::from_chatgpt_subscription_config(None);
    let auth = setup.request_auth_from_session(OpenAIChatGptSession {
        openai_api_key: "exchanged-api-key".to_string(),
        id_token: "id-token".to_string(),
        access_token: "oauth-access".to_string(),
        refresh_token: "refresh-token".to_string(),
        account_id: Some("acc_123".to_string()),
        email: Some("test@example.com".to_string()),
        plan: Some("plus".to_string()),
        obtained_at: 1,
        refreshed_at: 1,
        expires_at: None,
    });
    assert_eq!(auth.bearer_token, "oauth-access");
    assert_eq!(auth.chatgpt_account_id.as_deref(), Some("acc_123"));
    assert!(matches!(
        auth.rig_chatgpt_auth.as_ref(),
        Some(RigChatGptAuth::AccessToken {
            access_token,
            account_id: Some(account_id),
        }) if access_token == "oauth-access" && account_id.as_str() == "acc_123"
    ));

    let request = setup
        .authorize_request(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .expect("test client")
                .get("http://example.com"),
            &auth,
        )
        .build()
        .expect("request should build");

    assert_eq!(request.headers().get("authorization").and_then(|v| v.to_str().ok()), Some("Bearer oauth-access"));
    assert_eq!(request.headers().get("ChatGPT-Account-Id").and_then(|v| v.to_str().ok()), Some("acc_123"));
    assert_eq!(request.headers().get("originator").and_then(|v| v.to_str().ok()), Some("codex_cli_rs"));
    assert!(
        request.headers().get("session_id").is_none(),
        "ChatGPT subscription auth must not forward VT_SESSION_ID or any other conversation/session identifier"
    );
}

#[test]
fn api_key_and_chatgpt_subscription_share_responses_item_history_builder() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::system("Follow the local policy.".to_owned()),
            provider::Message::user("Summarize the repository state.".to_owned()),
            provider::Message::assistant("There is one modified file.".to_owned()),
            provider::Message::user("Continue.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_6.to_string(),
        stream: true,
        tools: Some(Arc::new(vec![sample_tool()])),
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());
    request.response_store = Some(true);
    request.responses_include = Some(vec!["output_text.annotations".to_owned()]);

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(
        native_payload.get("input"),
        chatgpt_payload.get("input"),
        "API-key and ChatGPT paths must share the same Responses item/history builder"
    );
    assert_eq!(native_payload.get("instructions"), chatgpt_payload.get("instructions"));
    assert_eq!(
        without_responses_backend_fields(native_payload.clone()),
        without_responses_backend_fields(chatgpt_payload.clone())
    );

    assert_absent(&native_payload, "previous_response_id");
    assert_absent(&chatgpt_payload, "previous_response_id");
    assert_eq!(native_payload.get("store").and_then(Value::as_bool), Some(false));
    assert_eq!(
        chatgpt_payload.get("store").and_then(Value::as_bool),
        Some(false),
        "ChatGPT subscription requests must force store=false"
    );
}

#[test]
fn openai_responses_native_emits_store_false_without_previous_response_id() {
    let provider = native_openai_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.previous_response_id = Some("resp_previous_123".to_string());

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");

    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "previous_response_id");
}

#[test]
fn chatgpt_responses_emits_store_false_without_previous_response_id() {
    let provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.previous_response_id = Some("resp_previous_123".to_string());

    let payload = provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_absent(&payload, "previous_response_id");
}

#[test]
fn openai_and_chatgpt_share_responses_payload_builder_except_backend() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::system("Follow the local policy.".to_owned()),
            provider::Message::user("Summarize the repository state.".to_owned()),
            provider::Message::assistant("There is one modified file.".to_owned()),
            provider::Message::user("Continue.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_6.to_string(),
        stream: true,
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());
    request.response_store = Some(true);
    request.prompt_cache_key = Some("vtcode:session".to_owned());

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(
        without_responses_backend_capability_fields(native_payload),
        without_responses_backend_capability_fields(chatgpt_payload)
    );
}

#[test]
fn openai_and_chatgpt_preserve_prompt_cache_key() {
    let native_provider = native_openai_provider(models::openai::GPT_5_6);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_6);
    let mut request = sample_request(models::openai::GPT_5_6);
    request.prompt_cache_key = Some("vtcode:session".to_owned());

    let native_payload = native_provider
        .convert_to_openai_responses_format(&request)
        .expect("native payload should build");
    let chatgpt_payload = chatgpt_provider
        .convert_to_openai_responses_format(&request)
        .expect("chatgpt payload should build");

    assert_eq!(native_payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:session"));
    assert_eq!(chatgpt_payload.get("prompt_cache_key").and_then(Value::as_str), Some("vtcode:session"));
}

#[test]
fn openai_and_chatgpt_replay_structured_history_on_continuation_turns() {
    let native_provider = native_openai_provider(models::openai::GPT_5_CODEX);
    let chatgpt_provider = chatgpt_backend_provider(models::openai::GPT_5_CODEX);
    let mut request = provider::LLMRequest {
        messages: vec![
            provider::Message::user("Inspect the workspace.".to_owned()),
            provider::Message::assistant_with_tools(
                "Running the requested check.".to_owned(),
                vec![provider::ToolCall::function(
                    "call_1".to_string(),
                    "exec_command".to_string(),
                    "{\"command\":\"cargo check\"}".to_string(),
                )],
            )
            .with_reasoning_details(Some(vec![json!({
                "type": "reasoning",
                "id": "rs_1",
                "encrypted_content": "opaque_reasoning",
                "summary": [{"type": "summary_text", "text": "checked command"}],
            })]))
            .with_phase(Some(provider::AssistantPhase::Commentary)),
            provider::Message::tool_response(
                "call_1".to_string(),
                "{\"output\":\"Finished `dev` profile\",\"exit_code\":0}".to_string(),
            ),
            provider::Message::assistant("The check completed successfully.".to_owned())
                .with_phase(Some(provider::AssistantPhase::FinalAnswer)),
            provider::Message::user("Continue with the next step.".to_owned()),
        ]
        .into(),
        model: models::openai::GPT_5_CODEX.to_string(),
        stream: true,
        ..Default::default()
    };
    request.previous_response_id = Some("resp_previous_123".to_owned());

    for payload in [
        native_provider
            .convert_to_openai_responses_format(&request)
            .expect("native payload should build"),
        chatgpt_provider
            .convert_to_openai_responses_format(&request)
            .expect("chatgpt payload should build"),
    ] {
        assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
        assert_absent(&payload, "previous_response_id");
        let input = get_input_array(&payload);
        assert!(
            input.iter().any(|item| item.get("role").and_then(Value::as_str) == Some("user")
                && item.to_string().contains("Inspect the workspace.")),
            "continuation replay must keep the first user turn, not only the suffix"
        );
        assert!(
            input.iter().any(|item| item.get("role").and_then(Value::as_str) == Some("user")
                && item.to_string().contains("Continue with the next step.")),
            "continuation replay must keep the latest user turn"
        );
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("encrypted_content").and_then(Value::as_str) == Some("opaque_reasoning")
        }));
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call")
                && item.get("call_id").and_then(Value::as_str) == Some("call_1")
        }));
        assert!(input.iter().any(|item| {
            item.get("type").and_then(Value::as_str) == Some("function_call_output")
                && item.get("call_id").and_then(Value::as_str) == Some("call_1")
        }));
    }
}
