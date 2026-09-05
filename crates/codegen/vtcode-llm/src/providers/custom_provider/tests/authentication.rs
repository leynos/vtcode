use super::super::CustomProviderBackendRouter;
use crate::provider::{LLMProvider, LLMRequest, Message};
use crate::providers::openai::CustomProviderAuthHandle;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use tempfile::TempDir;
use vtcode_config::constants::models;
use vtcode_config::core::{
    AnthropicConfig, CustomProviderApiFormat, CustomProviderCommandAuthConfig, CustomProviderConfig,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn write_tokens_file(dir: &Path, tokens: &[&str]) {
    std::fs::write(dir.join("tokens.txt"), tokens.join("\n")).expect("write tokens file");
}

#[cfg(unix)]
fn auth_fixture(dir: &TempDir, tokens: &[&str]) -> CustomProviderCommandAuthConfig {
    use std::os::unix::fs::PermissionsExt;

    write_tokens_file(dir.path(), tokens);
    let script_path = dir.path().join("print-token.sh");
    std::fs::write(
        &script_path,
        r#"#!/bin/sh
    first_line=$(sed -n '1p' tokens.txt)
    printf '%s\n' "$first_line"
    tail -n +2 tokens.txt > tokens.next
    mv tokens.next tokens.txt
    "#,
    )
    .expect("write script");
    let mut perms = std::fs::metadata(&script_path).expect("metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&script_path, perms).expect("set permissions");
    CustomProviderCommandAuthConfig {
        command: "./print-token.sh".to_string(),
        args: Vec::new(),
        cwd: Some(dir.path().to_path_buf()),
        timeout_ms: 5_000,
        refresh_interval_ms: 60_000,
    }
}

#[cfg(windows)]
fn auth_fixture(dir: &TempDir, tokens: &[&str]) -> CustomProviderCommandAuthConfig {
    write_tokens_file(dir.path(), tokens);
    let script_path = dir.path().join("print-token.ps1");
    std::fs::write(
        &script_path,
        r#"$lines = Get-Content -Path tokens.txt
    if ($lines.Count -eq 0) { exit 1 }
    Write-Output $lines[0]
    $lines | Select-Object -Skip 1 | Set-Content -Path tokens.txt
    "#,
    )
    .expect("write script");
    CustomProviderCommandAuthConfig {
        command: "powershell".to_string(),
        args: vec![
            "-NoProfile".to_string(),
            "-ExecutionPolicy".to_string(),
            "Bypass".to_string(),
            "-File".to_string(),
            script_path.to_string_lossy().into_owned(),
        ],
        cwd: Some(dir.path().to_path_buf()),
        timeout_ms: 5_000,
        refresh_interval_ms: 60_000,
    }
}

fn anthropic_response_body(text: &str) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": text,
        }],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 1,
            "output_tokens": 1,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0
        }
    })
}

#[tokio::test]
async fn anthropic_command_auth_refreshes_and_uses_protocol_headers() {
    let server = MockServer::start().await;
    let tempdir = TempDir::new().expect("tempdir");
    let seen_keys = Arc::new(Mutex::new(Vec::new()));
    let auth_config = auth_fixture(&tempdir, &["first-token", "second-token"]);

    let seen_for_mock = Arc::clone(&seen_keys);
    Mock::given(method("POST"))
        .and(path("/messages"))
        .respond_with(move |request: &wiremock::Request| {
            let api_key = request
                .headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_string();
            seen_for_mock.lock().expect("mutex").push(api_key);
            let count = seen_for_mock.lock().expect("mutex").len();
            if count == 1 {
                ResponseTemplate::new(401)
            } else {
                ResponseTemplate::new(200).set_body_json(anthropic_response_body("anthropic command auth"))
            }
        })
        .expect(2)
        .mount(&server)
        .await;

    let config = CustomProviderConfig {
        name: "anthropic-custom".to_string(),
        display_name: "Anthropic Custom".to_string(),
        base_url: server.uri(),
        api_format: CustomProviderApiFormat::AnthropicMessages,
        context_window: None,
        temperature: None,
        top_p: None,
        top_k: None,
        presence_penalty: None,
        frequency_penalty: None,
        max_tokens: None,
        reasoning_effort: None,
        supports_tools: None,
        supports_reasoning: None,
        supports_reasoning_effort: None,
        supports_vision: None,
        supports_structured_output: None,
        supports_parallel_tool_calls: None,
        supports_context_caching: None,
        supports_responses_compaction: None,
        supports_stream_usage: None,
        responses_allow_function_call_id_remap: None,
        supports_context_edits: Some(true),
        api_key_env: "ANTHROPIC_CUSTOM_API_KEY".to_string(),
        auth: None,
        model: models::anthropic::DEFAULT_MODEL.to_string(),
        models: vec![models::anthropic::DEFAULT_MODEL.to_string()],
        profiles: std::collections::BTreeMap::new(),
        pricing: Default::default(),
        rate_limit_headers: Default::default(),
        request_policy: Default::default(),
    };

    let router = CustomProviderBackendRouter::from_config(
        config,
        None,
        Some(models::anthropic::DEFAULT_MODEL.to_string()),
        server.uri(),
        None,
        None,
        None,
        Some(AnthropicConfig::default()),
        None,
        Some(CustomProviderAuthHandle::new(auth_config, None)),
    );

    let response = router
        .generate(LLMRequest {
            model: models::anthropic::DEFAULT_MODEL.to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        })
        .await
        .expect("anthropic auth should refresh and succeed");

    assert_eq!(response.content.as_deref(), Some("anthropic command auth"));
    assert_eq!(
        seen_keys.lock().expect("mutex").as_slice(),
        &["first-token".to_string(), "second-token".to_string()]
    );
}
