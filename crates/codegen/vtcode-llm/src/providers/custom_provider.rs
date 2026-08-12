use async_trait::async_trait;
use vtcode_commons::llm::BackendKind;
use vtcode_config::core::AnthropicConfig;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig, ResolvedCustomProviderProfile};
use vtcode_config::{ModelConfig, OpenAIConfig, PromptCachingConfig, TimeoutsConfig};

use crate::provider::{LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message, ResponsesCompactionOptions};
use crate::providers::anthropic::{self, AnthropicProvider};
use crate::providers::common::{resolve_model, validate_request_common};
use crate::providers::openai::{CustomProviderAuthHandle, OpenAIProvider};

/// Profile-aware backend router for custom providers.
pub struct CustomProviderBackendRouter {
    provider_key: String,
    display_name: String,
    default_model: String,
    supported_models: Vec<String>,
    custom_config: CustomProviderConfig,
    anthropic_config: AnthropicConfig,
    openai_auto: OpenAIProvider,
    openai_chat: OpenAIProvider,
    openai_responses: OpenAIProvider,
    anthropic: AnthropicProvider,
}

impl CustomProviderBackendRouter {
    #[allow(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    pub fn from_config(
        custom_config: CustomProviderConfig,
        api_key: Option<String>,
        model: Option<String>,
        base_url: String,
        prompt_cache: Option<PromptCachingConfig>,
        timeouts: Option<TimeoutsConfig>,
        openai: Option<OpenAIConfig>,
        anthropic: Option<AnthropicConfig>,
        model_behavior: Option<ModelConfig>,
        custom_provider_auth: Option<CustomProviderAuthHandle>,
    ) -> Self {
        let provider_key = custom_config.name.clone();
        let display_name = custom_config.display_name.clone();
        let supported_models = custom_config.effective_models();
        let configured_default = resolve_model(model, &custom_config.model);
        let default_model = if configured_default.trim().is_empty() {
            supported_models.first().cloned().unwrap_or_default()
        } else {
            configured_default
        };
        let anthropic_config = anthropic.unwrap_or_default();

        let openai_auto = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behavior.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_custom_provider_config(custom_config.clone());
        let openai_chat = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behavior.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_api_format_override(Some(CustomProviderApiFormat::OpenAIChat))
        .with_custom_provider_config(custom_config.clone());
        let openai_responses = OpenAIProvider::from_custom_config(
            provider_key.clone(),
            display_name.clone(),
            api_key.clone(),
            Some(default_model.clone()),
            Some(base_url.clone()),
            prompt_cache.clone(),
            timeouts.clone(),
            openai.clone(),
            model_behavior.clone(),
            custom_provider_auth.clone(),
            Some(supported_models.clone()),
        )
        .with_api_format_override(Some(CustomProviderApiFormat::OpenAIResponses))
        .with_custom_provider_config(custom_config.clone());
        let anthropic = AnthropicProvider::from_config(
            api_key,
            Some(default_model.clone()),
            Some(base_url),
            prompt_cache,
            timeouts,
            Some(anthropic_config.clone()),
            model_behavior,
        )
        .with_custom_auth(custom_provider_auth);

        Self {
            provider_key,
            display_name,
            default_model,
            supported_models,
            custom_config,
            anthropic_config,
            openai_auto,
            openai_chat,
            openai_responses,
            anthropic,
        }
    }

    fn resolved_model<'a>(&'a self, model: &'a str) -> &'a str {
        if model.trim().is_empty() {
            &self.default_model
        } else {
            model
        }
    }

    fn profile_for_model(&self, model: &str) -> ResolvedCustomProviderProfile {
        self.custom_config.resolved_profile(model)
    }

    fn backend_for_model(&self, model: &str) -> &dyn LLMProvider {
        match self.profile_for_model(self.resolved_model(model)).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => &self.anthropic,
            Some(CustomProviderApiFormat::OpenAIChat) => &self.openai_chat,
            Some(CustomProviderApiFormat::OpenAIResponses) => &self.openai_responses,
            Some(CustomProviderApiFormat::Auto) | None => &self.openai_auto,
        }
    }

    fn override_bool(value: Option<bool>, default: bool) -> bool {
        value.unwrap_or(default)
    }
}

#[async_trait]
impl LLMProvider for CustomProviderBackendRouter {
    fn name(&self) -> &str {
        &self.provider_key
    }

    fn backend_kind(&self) -> BackendKind {
        self.backend_for_model(&self.default_model).backend_kind()
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_non_streaming(&self, model: &str) -> bool {
        self.backend_for_model(model).supports_non_streaming(self.resolved_model(model))
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_reasoning, self.backend_for_model(model).supports_reasoning(resolved))
    }

    fn supports_reasoning_effort(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_reasoning_effort,
            self.backend_for_model(model).supports_reasoning_effort(resolved),
        )
    }

    fn supports_tools(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_tools, self.backend_for_model(model).supports_tools(resolved))
    }

    fn supports_parallel_tool_config(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_parallel_tool_calls,
            self.backend_for_model(model).supports_parallel_tool_config(resolved),
        )
    }

    fn supports_structured_output(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_structured_output,
            self.backend_for_model(model).supports_structured_output(resolved),
        )
    }

    fn supports_context_caching(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_caching,
            self.backend_for_model(model).supports_context_caching(resolved),
        )
    }

    fn supports_vision(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(profile.supports_vision, self.backend_for_model(model).supports_vision(resolved))
    }

    fn supports_responses_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        let backend_default = match self.profile_for_model(resolved).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => false,
            _ => self.backend_for_model(model).supports_responses_compaction(resolved),
        };
        Self::override_bool(profile.supports_responses_compaction, backend_default)
    }

    fn supports_native_allowed_tools(&self, model: &str) -> bool {
        self.backend_for_model(model)
            .supports_native_allowed_tools(self.resolved_model(model))
    }

    fn supports_context_edits(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_edits,
            self.backend_for_model(model).supports_context_edits(resolved),
        )
    }

    fn supports_manual_openai_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        let backend_default = match self.profile_for_model(resolved).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => false,
            _ => self.backend_for_model(model).supports_manual_openai_compaction(resolved),
        };
        Self::override_bool(profile.supports_responses_compaction, backend_default)
    }

    fn supports_native_inline_compaction(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        Self::override_bool(
            profile.supports_context_edits,
            self.backend_for_model(model).supports_native_inline_compaction(resolved),
        )
    }

    fn manual_openai_compaction_unavailable_message(&self, model: &str) -> String {
        self.backend_for_model(model)
            .manual_openai_compaction_unavailable_message(self.resolved_model(model))
    }

    fn effective_context_size(&self, model: &str) -> usize {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        profile
            .context_window
            .unwrap_or_else(|| self.backend_for_model(model).effective_context_size(resolved))
    }

    async fn compact_history(&self, model: &str, history: &[Message]) -> Result<Vec<Message>, LLMError> {
        self.backend_for_model(model)
            .compact_history(self.resolved_model(model), history)
            .await
    }

    async fn compact_history_with_options(
        &self,
        model: &str,
        history: &[Message],
        options: &ResponsesCompactionOptions,
    ) -> Result<Vec<Message>, LLMError> {
        self.backend_for_model(model)
            .compact_history_with_options(self.resolved_model(model), history, options)
            .await
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        self.backend_for_model(&request.model).stream(request).await
    }

    async fn stream_normalized(&self, request: LLMRequest) -> Result<crate::provider::LLMNormalizedStream, LLMError> {
        self.backend_for_model(&request.model).stream_normalized(request).await
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        self.backend_for_model(&request.model).generate(request).await
    }

    fn supported_models(&self) -> Vec<String> {
        self.supported_models.clone()
    }

    fn validate_request(&self, request: &LLMRequest) -> Result<(), LLMError> {
        let resolved_model = self.resolved_model(&request.model);
        match self.profile_for_model(resolved_model).api_format {
            Some(CustomProviderApiFormat::AnthropicMessages) => {
                validate_request_common(request, &self.display_name, &self.provider_key, Some(&self.supported_models))?;
                anthropic::validation::validate_request(
                    request,
                    &self.default_model,
                    &self.anthropic_config,
                    &self.display_name,
                )
            }
            Some(CustomProviderApiFormat::OpenAIChat)
            | Some(CustomProviderApiFormat::OpenAIResponses)
            | Some(CustomProviderApiFormat::Auto)
            | None => self.backend_for_model(resolved_model).validate_request(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::CustomProviderBackendRouter;
    use crate::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message, ToolCall, ToolDefinition};
    use crate::providers::openai::CustomProviderAuthHandle;
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use vtcode_config::constants::models;
    use vtcode_config::core::{
        AnthropicConfig, CustomProviderApiFormat, CustomProviderCommandAuthConfig, CustomProviderConfig,
        CustomProviderProfileConfig,
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
            supports_tools: None,
            supports_reasoning: None,
            supports_reasoning_effort: None,
            supports_vision: None,
            supports_structured_output: None,
            supports_parallel_tool_calls: None,
            supports_context_caching: None,
            supports_responses_compaction: None,
            supports_context_edits: Some(true),
            api_key_env: "ANTHROPIC_CUSTOM_API_KEY".to_string(),
            auth: None,
            model: models::anthropic::DEFAULT_MODEL.to_string(),
            models: vec![models::anthropic::DEFAULT_MODEL.to_string()],
            profiles: std::collections::BTreeMap::new(),
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

    #[tokio::test]
    async fn openai_chat_custom_profile_preserves_reasoning_and_serialises_controls() {
        let server = MockServer::start().await;
        let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let captured_for_mock = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                *captured_for_mock.lock().expect("capture mutex") = serde_json::from_slice(&request.body).ok();
                ResponseTemplate::new(200).set_body_json(json!({
                    "choices": [{
                        "message": {
                            "content": "answer",
                            "reasoning_content": "think first"
                        },
                        "finish_reason": "stop"
                    }]
                }))
            })
            .mount(&server)
            .await;

        let model = "DeepSeek-V4-Flash-0731".to_owned();
        let mut config = CustomProviderConfig {
            name: "deepseek-custom".to_owned(),
            display_name: "DeepSeek Custom".to_owned(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: model.clone(),
            models: vec![model.clone()],
            ..Default::default()
        };
        config.profiles.insert(
            model.clone(),
            CustomProviderProfileConfig {
                supports_reasoning: Some(true),
                supports_reasoning_effort: Some(true),
                ..Default::default()
            },
        );

        let provider = CustomProviderBackendRouter::from_config(
            config,
            Some("test-key".to_owned()),
            Some(model.clone()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );
        let response = provider
            .generate(LLMRequest {
                model,
                messages: vec![Message::user("hello".to_owned())].into(),
                reasoning_effort: Some(vtcode_config::types::ReasoningEffortLevel::High),
                ..Default::default()
            })
            .await
            .expect("custom OpenAI chat response should parse");

        assert_eq!(response.reasoning.as_deref(), Some("think first"));
        let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
        assert_eq!(payload["include_reasoning"], true);
        assert_eq!(payload["reasoning_effort"], "high");
    }

    #[tokio::test]
    async fn openai_chat_stream_reassembles_fragmented_tool_call_playback() {
        let server = MockServer::start().await;
        let captured: Arc<Mutex<Option<Value>>> = Arc::new(Mutex::new(None));
        let captured_for_mock = Arc::clone(&captured);
        let body = concat!(
            "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Checking.\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"list_files\",\"arguments\":\"{\\\"pa\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"th\\\":\\\"\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"chatcmpl-tool\",\"model\":\"fixture-model\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                *captured_for_mock.lock().expect("capture mutex") = serde_json::from_slice(&request.body).ok();
                ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
            })
            .expect(1)
            .mount(&server)
            .await;

        let model = "fixture-model".to_owned();
        let config = CustomProviderConfig {
            name: "streaming-custom".to_owned(),
            display_name: "Streaming Custom".to_owned(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: model.clone(),
            models: vec![model.clone()],
            supports_tools: Some(true),
            ..Default::default()
        };
        let provider = CustomProviderBackendRouter::from_config(
            config,
            Some("test-key".to_owned()),
            Some(model.clone()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );
        let tools = vec![ToolDefinition::function(
            "list_files".to_owned(),
            "List workspace files".to_owned(),
            json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        )];
        let mut stream = provider
            .stream(LLMRequest {
                model,
                messages: vec![Message::user("inspect the workspace".to_owned())].into(),
                stream: true,
                tools: Some(Arc::new(tools)),
                ..Default::default()
            })
            .await
            .expect("custom OpenAI chat stream should start");

        let mut completed = None;
        while let Some(event) = stream.next().await {
            if let LLMStreamEvent::Completed { response } = event.expect("stream event should decode") {
                completed = Some(*response);
            }
        }

        let response = completed.expect("stream should complete");
        assert_eq!(response.content.as_deref(), Some("Checking."));
        assert_eq!(
            response.tool_calls,
            Some(vec![ToolCall::function(
                "call_1".to_owned(),
                "list_files".to_owned(),
                r#"{"path":""}"#.to_owned(),
            )]),
        );
        let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["tools"][0]["function"]["name"], "list_files");
    }

    #[tokio::test]
    async fn openai_chat_stream_rejects_eof_without_a_terminal_frame() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                concat!(
                    "data: {\"id\":\"chatcmpl-truncated\",\"choices\":[{\"index\":0,",
                    "\"delta\":{\"content\":\"partial\"},\"finish_reason\":null}]}\n\n",
                ),
                "text/event-stream",
            ))
            .expect(1)
            .mount(&server)
            .await;

        let model = "fixture-model".to_owned();
        let config = CustomProviderConfig {
            name: "truncated-custom".to_owned(),
            display_name: "Truncated Custom".to_owned(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: model.clone(),
            models: vec![model.clone()],
            ..Default::default()
        };
        let provider = CustomProviderBackendRouter::from_config(
            config,
            Some("test-key".to_owned()),
            Some(model.clone()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );
        let mut stream = provider
            .stream(LLMRequest {
                model,
                messages: vec![Message::user("start then disconnect".to_owned())].into(),
                stream: true,
                ..Default::default()
            })
            .await
            .expect("truncated response should establish its stream");

        assert!(matches!(
            stream.next().await,
            Some(Ok(LLMStreamEvent::Token { delta })) if delta == "partial"
        ));
        assert!(matches!(stream.next().await, Some(Err(_))), "truncated stream should fail");
        assert!(stream.next().await.is_none(), "failed stream should then terminate");
    }
}
