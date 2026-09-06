//! Streaming, retry, and token-refresh request tests.

use super::*;

#[tokio::test]
async fn chatgpt_backend_uses_oauth_access_token_and_account_header() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let provider = OpenAIProvider::new_with_client(
        "api-key".to_string(),
        Some(sample_chatgpt_auth_handle()),
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );
    assert!(matches!(
        provider.backend_setup.kind(),
        OpenAIBackendKind::ChatGptSubscription(ChatGptSubscriptionAuthSource::RigChatGpt)
    ));

    let auth = provider.request_auth_from_session(OpenAIChatGptSession {
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

    let request = provider
        .authorize_with_api_key(provider.http_client.get("http://example.com"), &auth)
        .build()
        .expect("request should build");
    assert_eq!(request.headers().get("authorization").and_then(|v| v.to_str().ok()), Some("Bearer oauth-access"));
    assert_eq!(request.headers().get("ChatGPT-Account-Id").and_then(|v| v.to_str().ok()), Some("acc_123"));
}

#[tokio::test]
async fn api_key_responses_stream_sends_metadata_and_preserves_usage() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let stream_body = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from api key stream\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_api_stream\",\"output\":[],\"usage\":{\"input_tokens\":21,\"output_tokens\":6,\"total_tokens\":27,\"input_tokens_details\":{\"cached_tokens\":8}}}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("valid json");
            *captured_for_mock.lock().expect("not poisoned") = Some(json!({
                "body": body,
                "beta": req.headers.get("openai-beta").and_then(|v| v.to_str().ok()),
                "client_request_id": req.headers.get("x-client-request-id").and_then(|v| v.to_str().ok()),
                "turn_metadata": req.headers.get("x-turn-metadata").and_then(|v| v.to_str().ok()),
            }));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body)
        })
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        "test-key".to_string(),
        None,
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        native_openai_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let mut stream = provider
        .stream(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5.to_string(),
            response_store: Some(true),
            metadata: Some(json!({"commit": "abc123"})),
            ..Default::default()
        })
        .await
        .expect("stream should start");
    let mut text = String::new();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        match event.expect("stream event should parse") {
            provider::LLMStreamEvent::Token { delta } => text.push_str(&delta),
            provider::LLMStreamEvent::Completed { response } => {
                completed = Some(*response);
                break;
            }
            provider::LLMStreamEvent::Reasoning { .. }
            | provider::LLMStreamEvent::ReasoningSignature { .. }
            | provider::LLMStreamEvent::ReasoningStage { .. } => {}
        }
    }

    let response = completed.expect("completion event should be emitted");
    assert_eq!(text, "hello from api key stream");
    assert_eq!(response.content.as_deref(), Some("hello from api key stream"));
    assert_eq!(response.request_id.as_deref(), Some("resp_api_stream"));
    let usage = response.usage.expect("final usage should be preserved");
    assert_eq!(usage.prompt_tokens, 21);
    assert_eq!(usage.completion_tokens, 6);
    assert_eq!(usage.total_tokens, 27);
    assert_eq!(usage.cached_prompt_tokens, None);

    let captured = captured.lock().expect("not poisoned").clone().expect("request captured");
    assert_eq!(captured.get("beta").and_then(Value::as_str), Some("responses=v1"));
    assert!(
        captured
            .get("client_request_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with("vtcode-"))
    );
    assert_eq!(captured.get("turn_metadata").and_then(Value::as_str), Some(r#"{"commit":"abc123"}"#));
    let payload = captured.get("body").expect("body captured");
    assert_eq!(payload.get("stream").and_then(Value::as_bool), Some(true));
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_ne!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));
}

#[tokio::test]
async fn chatgpt_responses_stream_accepts_empty_final_output_after_text_delta() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let captured = Arc::new(Mutex::new(None::<Value>));
    let captured_for_mock = Arc::clone(&captured);
    let stream_body = concat!(
        "data: {\"type\":\"response.queued\",\"response\":{\"id\":\"resp_chatgpt_stream\",\"status\":\"queued\"}}\n\n",
        "data: {\"type\":\"response.file_search_call.searching\",\"item_id\":\"fs_1\",\"output_index\":0}\n\n",
        "data: {\"type\":\"response.code_interpreter_call_code.delta\",\"item_id\":\"ci_1\",\"output_index\":1,\"sequence_number\":1,\"delta\":\"print(1)\"}\n\n",
        "data: {\"type\":\"response.mcp_call_arguments.delta\",\"item_id\":\"mcp_1\",\"call_id\":\"call_mcp\",\"output_index\":2,\"sequence_number\":2,\"delta\":\"{\\\"query\\\":\\\"vtcode\\\"}\"}\n\n",
        "data: {\"type\":\"response.image_generation_call.partial_image\",\"item_id\":\"img_1\",\"output_index\":3,\"sequence_number\":3,\"partial_image_b64\":\"ZmFrZQ==\"}\n\n",
        "data: {\"type\":\"response.custom_tool_call_input.done\",\"item_id\":\"custom_1\",\"call_id\":\"call_custom\",\"output_index\":4,\"sequence_number\":4,\"input\":\"{\\\"cmd\\\":\\\"test\\\"}\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello from stream\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_chatgpt_stream\",\"output\":[],\"usage\":{\"input_tokens\":13,\"output_tokens\":4,\"total_tokens\":17}}}\n\n",
    );

    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(move |req: &wiremock::Request| {
            let body: Value = serde_json::from_slice(&req.body).expect("valid json");
            *captured_for_mock.lock().expect("not poisoned") = Some(json!({
                "body": body,
                "account": req.headers.get("chatgpt-account-id").and_then(|v| v.to_str().ok()),
                "originator": req.headers.get("originator").and_then(|v| v.to_str().ok()),
                "beta": req.headers.get("openai-beta").and_then(|v| v.to_str().ok()),
                "has_session_id": req.headers.contains_key("session_id"),
            }));
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(stream_body)
        })
        .expect(1)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        String::new(),
        Some(sample_chatgpt_auth_handle()),
        models::openai::GPT_5_6_SOL.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let mut stream = provider
        .stream(provider::LLMRequest {
            messages: vec![provider::Message::user("Hello".to_string())].into(),
            model: models::openai::GPT_5_6_SOL.to_string(),
            ..Default::default()
        })
        .await
        .expect("stream should start");
    let mut text = String::new();
    let mut completed = None;
    while let Some(event) = stream.next().await {
        match event.expect("stream event should parse") {
            provider::LLMStreamEvent::Token { delta } => text.push_str(&delta),
            provider::LLMStreamEvent::Completed { response } => {
                completed = Some(*response);
                break;
            }
            provider::LLMStreamEvent::Reasoning { .. }
            | provider::LLMStreamEvent::ReasoningSignature { .. }
            | provider::LLMStreamEvent::ReasoningStage { .. } => {}
        }
    }

    let response = completed.expect("completion event should be emitted");
    assert_eq!(text, "hello from stream");
    assert_eq!(response.content.as_deref(), Some("hello from stream"));
    assert_eq!(response.request_id.as_deref(), Some("resp_chatgpt_stream"));
    let usage = response.usage.expect("final usage should be preserved");
    assert_eq!(usage.prompt_tokens, 13);
    assert_eq!(usage.completion_tokens, 4);
    assert_eq!(usage.total_tokens, 17);

    let payload = captured.lock().expect("not poisoned").clone().expect("payload captured");
    assert_eq!(payload.get("account").and_then(Value::as_str), Some("acc_123"));
    assert_eq!(payload.get("originator").and_then(Value::as_str), Some("codex_cli_rs"));
    assert_eq!(payload.get("beta").and_then(Value::as_str), Some("responses=v1"));
    assert_eq!(
        payload.get("has_session_id").and_then(Value::as_bool),
        Some(false),
        "ChatGPT Responses requests must not include a session_id header"
    );
    let payload = payload.get("body").expect("body captured");
    assert_eq!(payload.get("store").and_then(Value::as_bool), Some(false));
    assert_eq!(payload.get("include").and_then(Value::as_array), Some(&vec![json!("reasoning.encrypted_content")]));
}

#[tokio::test]
async fn external_chatgpt_auth_retries_with_refreshed_tokens_after_401() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let refresh_calls = Arc::new(Mutex::new(0usize));
    let seen_bearer_tokens = Arc::new(Mutex::new(Vec::new()));

    Mock::given(method("GET"))
        .and(path("/auth-retry"))
        .respond_with(mock_retry_by_call_count(Arc::clone(&seen_bearer_tokens)))
        .expect(2)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::new_with_client(
        "api-key".to_string(),
        Some(OpenAIChatGptAuthHandle::new_external(
            OpenAIChatGptSession {
                openai_api_key: String::new(),
                id_token: "id-token".to_string(),
                access_token: "oauth-access".to_string(),
                refresh_token: String::new(),
                account_id: Some("acc_123".to_string()),
                email: Some("test@example.com".to_string()),
                plan: Some("plus".to_string()),
                obtained_at: 1,
                refreshed_at: u64::MAX / 2,
                expires_at: None,
            },
            true,
            Arc::new(ExternalSessionRefresher { calls: Arc::clone(&refresh_calls) }),
        )),
        models::openai::GPT_5.to_string(),
        reqwest::Client::builder().no_proxy().build().expect("test client"),
        chatgpt_mock_base_url(&server),
        TimeoutsConfig::default(),
    );

    let response = provider
        .send_authorized(|auth| {
            provider
                .authorize_with_api_key(provider.http_client.get(format!("{}{}", server.uri(), "/auth-retry")), auth)
        })
        .await
        .expect("request should succeed after refresh");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        seen_bearer_tokens.lock().expect("mutex not poisoned").as_slice(),
        &[
            "Bearer oauth-access".to_string(),
            "Bearer oauth-access-refreshed".to_string()
        ]
    );
    assert_eq!(*refresh_calls.lock().expect("mutex not poisoned"), 1);
}

#[tokio::test]
async fn custom_provider_auth_retries_with_refreshed_tokens_after_401() {
    let Some(server) = start_mock_server_or_skip().await else {
        return;
    };
    let tempdir = TempDir::new().expect("tempdir");
    let seen_bearer_tokens = Arc::new(Mutex::new(Vec::new()));

    Mock::given(method("GET"))
        .and(path("/custom-auth-retry"))
        .respond_with(mock_retry_by_call_count(Arc::clone(&seen_bearer_tokens)))
        .expect(2)
        .mount(&server)
        .await;

    let provider = OpenAIProvider::from_custom_config(
        "mycorp".to_string(),
        "MyCorp".to_string(),
        None,
        Some(models::openai::GPT_5.to_string()),
        Some(native_openai_mock_base_url(&server)),
        None,
        None,
        None,
        None,
        Some(CustomProviderAuthHandle::new(
            custom_provider_auth_fixture(&tempdir, &["first-token", "second-token"]),
            None,
        )),
        None,
    );

    let response = provider
        .send_authorized(|auth| {
            provider.authorize_with_api_key(
                provider.http_client.get(format!("{}{}", server.uri(), "/custom-auth-retry")),
                auth,
            )
        })
        .await
        .expect("request should succeed after command-auth refresh");

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        seen_bearer_tokens.lock().expect("mutex not poisoned").as_slice(),
        &["Bearer first-token".to_string(), "Bearer second-token".to_string()]
    );
}
