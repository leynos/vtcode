use async_trait::async_trait;
use vtcode_commons::llm::BackendKind;
use vtcode_config::core::AnthropicConfig;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig, ResolvedCustomProviderProfile};
use vtcode_config::{ModelConfig, OpenAIConfig, PromptCachingConfig, TimeoutsConfig};

use crate::provider::{
    LLMError, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message, ResponsesCompactionOptions, SamplingOverrides,
};
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
        .with_custom_auth(custom_provider_auth)
        .with_rate_limit_headers(custom_config.effective_rate_limit_headers());

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
        // A pin in THIS model's own profile implies effort support unless the
        // same profile explicitly disables it. Provider-level defaults — both
        // the effort value and the capability flag — must not flip support for
        // every model on the endpoint.
        let own = self.custom_config.profiles.get(resolved);
        let own_pin = own.and_then(|p| p.reasoning_effort).is_some();
        if own_pin {
            return own.and_then(|p| p.supports_reasoning_effort) != Some(false);
        }
        Self::override_bool(
            profile.supports_reasoning_effort,
            self.backend_for_model(model).supports_reasoning_effort(resolved),
        )
    }

    fn sampling_overrides(&self, model: &str) -> SamplingOverrides {
        let resolved = self.resolved_model(model);
        let profile = self.profile_for_model(resolved);
        SamplingOverrides {
            temperature: profile.temperature,
            top_p: profile.top_p,
            top_k: profile.top_k,
            presence_penalty: profile.presence_penalty,
            frequency_penalty: profile.frequency_penalty,
            max_tokens: profile.max_tokens,
            reasoning_effort: profile.reasoning_effort,
            suppresses_sampling_with_reasoning: profile.api_format == Some(CustomProviderApiFormat::AnthropicMessages),
            profile_aware: true,
        }
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

    fn supports_turn_scoped_system_messages(&self, model: &str) -> bool {
        let resolved = self.resolved_model(model);
        matches!(self.profile_for_model(resolved).api_format, Some(CustomProviderApiFormat::AnthropicMessages))
            && self.backend_for_model(model).supports_turn_scoped_system_messages(resolved)
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
                // Anthropic-shaped custom profiles use the Anthropic message
                // contract, including its turn-scoped system marker. Keep the
                // custom display name for diagnostics, but validate message roles
                // against the backend contract so the marker is not rejected.
                validate_request_common(request, &self.display_name, "anthropic", Some(&self.supported_models))?;
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
    use crate::provider::{
        LLMError, LLMProvider, LLMRequest, LLMStreamEvent, Message, SamplingOverrides, ToolCall, ToolDefinition,
    };
    use crate::providers::openai::CustomProviderAuthHandle;
    use futures::StreamExt;
    use serde_json::{Value, json};
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use tempfile::TempDir;
    use vtcode_config::constants::models;
    use vtcode_config::core::{
        AnthropicConfig, CustomProviderApiFormat, CustomProviderCommandAuthConfig, CustomProviderConfig,
        CustomProviderProfileConfig, RateLimitHeaderConfig,
    };
    use vtcode_config::types::ReasoningEffortLevel;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sampling_router() -> CustomProviderBackendRouter {
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            "cold-model".to_string(),
            CustomProviderProfileConfig {
                temperature: Some(0.0),
                top_p: Some(0.9),
                reasoning_effort: Some(ReasoningEffortLevel::Low),
                ..Default::default()
            },
        );

        CustomProviderBackendRouter::from_config(
            CustomProviderConfig {
                name: "sampling-test".to_string(),
                display_name: "Sampling Test".to_string(),
                base_url: "https://llm.example/v1".to_string(),
                temperature: Some(0.5),
                models: vec!["cold-model".to_string(), "warm-model".to_string()],
                profiles,
                ..Default::default()
            },
            None,
            None,
            "https://llm.example/v1".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    #[test]
    fn sampling_overrides_resolve_per_model_with_provider_defaults() {
        let router = sampling_router();

        let cold = router.sampling_overrides("cold-model");
        assert_eq!(
            cold,
            SamplingOverrides {
                temperature: Some(0.0),
                top_p: Some(0.9),
                top_k: None,
                presence_penalty: None,
                frequency_penalty: None,
                max_tokens: None,
                reasoning_effort: Some(ReasoningEffortLevel::Low),
                suppresses_sampling_with_reasoning: false,
                profile_aware: true,
            }
        );
        assert!(router.supports_reasoning_effort("cold-model"));

        // Models without a profile fall back to provider-level defaults.
        let warm = router.sampling_overrides("warm-model");
        assert_eq!(warm.temperature, Some(0.5));
        assert_eq!(warm.top_p, None);
    }

    #[test]
    fn sampling_overrides_suppression_matrix_matches_native_semantics() {
        let profile_openai = SamplingOverrides { profile_aware: true, ..Default::default() };
        let profile_anthropic = SamplingOverrides {
            suppresses_sampling_with_reasoning: true,
            profile_aware: true,
            ..Default::default()
        };
        let builtin_default = SamplingOverrides::default();

        // Custom openai-shaped profile keeps pinned values during reasoning.
        assert!(!profile_openai.suppresses_sampling(false, true));

        // Custom anthropic-messages profile drops them.
        assert!(profile_anthropic.suppresses_sampling(false, true));
        assert!(!profile_anthropic.suppresses_sampling(false, false));

        // Built-in Anthropic/MiniMax shape suppresses only via native match;
        // built-in OpenAI shape never does through overrides alone.
        assert!(builtin_default.suppresses_sampling(true, true));
        assert!(!builtin_default.suppresses_sampling(true, false));
        assert!(!builtin_default.suppresses_sampling(false, true));

        // Level "none"/"unknown" is not active reasoning anywhere.
        assert!(!profile_anthropic.suppresses_sampling(false, false));
    }

    #[test]
    fn turn_scoped_system_capability_follows_selected_api_profile() {
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            models::anthropic::CLAUDE_FABLE_5.to_string(),
            CustomProviderProfileConfig {
                api_format: CustomProviderApiFormat::AnthropicMessages,
                ..Default::default()
            },
        );

        let router = CustomProviderBackendRouter::from_config(
            CustomProviderConfig {
                name: "profiled-transport-test".to_string(),
                display_name: "Profiled Transport Test".to_string(),
                base_url: "https://llm.example/v1".to_string(),
                model: models::anthropic::CLAUDE_FABLE_5.to_string(),
                models: vec![
                    models::anthropic::CLAUDE_FABLE_5.to_string(),
                    "openai-shaped".to_string(),
                ],
                profiles,
                ..Default::default()
            },
            Some("test-key".to_string()),
            None,
            "https://llm.example/v1".to_string(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );

        assert!(router.supports_turn_scoped_system_messages(models::anthropic::CLAUDE_FABLE_5));
        assert!(!router.supports_turn_scoped_system_messages("openai-shaped"));
    }

    #[test]
    fn anthropic_profile_accepts_turn_scoped_system_marker() {
        let mut profiles = std::collections::BTreeMap::new();
        profiles.insert(
            models::anthropic::CLAUDE_FABLE_5.to_string(),
            CustomProviderProfileConfig {
                api_format: CustomProviderApiFormat::AnthropicMessages,
                ..Default::default()
            },
        );
        let router = CustomProviderBackendRouter::from_config(
            CustomProviderConfig {
                name: "profiled-validation-test".to_string(),
                display_name: "Profiled Validation Test".to_string(),
                base_url: "https://llm.example/v1".to_string(),
                model: models::anthropic::CLAUDE_FABLE_5.to_string(),
                models: vec![models::anthropic::CLAUDE_FABLE_5.to_string()],
                profiles,
                ..Default::default()
            },
            Some("test-key".to_string()),
            None,
            "https://llm.example/v1".to_string(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );
        let request = LLMRequest {
            model: models::anthropic::CLAUDE_FABLE_5.to_string(),
            messages: vec![Message::turn_scoped_system("visible-output notice".to_string())].into(),
            ..Default::default()
        };

        router
            .validate_request(&request)
            .expect("custom Anthropic profile should accept native marker");
    }

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

    fn assert_custom_rate_limit(error: LLMError) {
        let LLMError::RateLimit { metadata: Some(metadata) } = error else {
            panic!("expected custom provider rate-limit metadata");
        };
        assert_eq!(metadata.retry_after.as_deref(), Some("4"));
        let rate_limit = metadata.rate_limit.expect("configured rate-limit headers");
        assert_eq!(rate_limit.requests_remaining_per_minute, Some(2));
        assert_eq!(rate_limit.tokens_limit_per_second, Some(900));
        assert_eq!(rate_limit.reset_after_millis, Some(1_250));
    }

    #[tokio::test]
    async fn openai_chat_streaming_and_buffered_errors_preserve_configured_rate_limit_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "4")
                    .insert_header("x-corp-requests-remaining", "2")
                    .insert_header("x-corp-token-limit", "900")
                    .insert_header("x-corp-reset", "1.25")
                    .insert_header("x-corp-private", "must-not-escape")
                    .set_body_json(json!({"error": {"message": "slow down"}})),
            )
            .expect(2)
            .mount(&server)
            .await;

        let model = "corp-model".to_string();
        let config = CustomProviderConfig {
            name: "mycorp".to_string(),
            display_name: "MyCorp".to_string(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            model: model.clone(),
            models: vec![model.clone()],
            rate_limit_headers: RateLimitHeaderConfig {
                requests_remaining_per_minute: Some("x-corp-requests-remaining".to_string()),
                tokens_limit_per_second: Some("x-corp-token-limit".to_string()),
                reset_after_seconds: Some("x-corp-reset".to_string()),
                ..RateLimitHeaderConfig::default()
            },
            ..Default::default()
        };
        let provider = CustomProviderBackendRouter::from_config(
            config,
            Some("fixture-key".to_string()),
            Some(model.clone()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );
        let request = || LLMRequest {
            model: model.clone(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        };

        let stream_error = match provider.stream(request()).await {
            Ok(_) => panic!("streaming request should be limited"),
            Err(error) => error,
        };
        assert_custom_rate_limit(stream_error);
        assert_custom_rate_limit(
            provider
                .generate(request())
                .await
                .expect_err("buffered request should be limited"),
        );
    }

    #[tokio::test]
    async fn openai_chat_retryable_provider_error_always_preserves_retry_after() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(503)
                    .insert_header("retry-after", "6")
                    .set_body_json(json!({"error": {"message": "temporarily unavailable"}})),
            )
            .mount(&server)
            .await;

        let model = "corp-model".to_string();
        let provider = CustomProviderBackendRouter::from_config(
            CustomProviderConfig {
                name: "mycorp".to_string(),
                display_name: "MyCorp".to_string(),
                base_url: server.uri(),
                api_format: CustomProviderApiFormat::OpenAIChat,
                model: model.clone(),
                models: vec![model.clone()],
                rate_limit_headers: RateLimitHeaderConfig {
                    prompt_tokens: Some("x-unrelated-counter".to_string()),
                    ..RateLimitHeaderConfig::default()
                },
                ..Default::default()
            },
            Some("fixture-key".to_string()),
            Some(model.clone()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );

        let error = provider
            .generate(LLMRequest {
                model,
                messages: vec![Message::user("hello".to_string())].into(),
                ..Default::default()
            })
            .await
            .expect_err("503 should remain a provider error");
        let LLMError::Provider { metadata: Some(metadata), .. } = error else {
            panic!("expected provider error metadata");
        };
        assert_eq!(metadata.retry_after.as_deref(), Some("6"));
        assert!(metadata.rate_limit.is_none());
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
                reasoning_effort: Some(ReasoningEffortLevel::High),
                ..Default::default()
            })
            .await
            .expect("custom OpenAI chat response should parse");

        assert_eq!(response.reasoning.as_deref(), Some("think first"));
        let payload = captured.lock().expect("capture mutex").clone().expect("request captured");
        assert_eq!(payload["include_reasoning"], true);
        assert_eq!(payload["reasoning_effort"], "high");
    }

    async fn collect_completed_response(
        provider: &CustomProviderBackendRouter,
        model: &str,
    ) -> crate::provider::LLMResponse {
        let mut stream = provider
            .stream(LLMRequest {
                model: model.to_string(),
                messages: vec![Message::user("hello".to_string())].into(),
                stream: true,
                ..Default::default()
            })
            .await
            .expect("stream should start");

        while let Some(event) = stream.next().await {
            if let LLMStreamEvent::Completed { response } = event.expect("stream event should decode") {
                return *response;
            }
        }

        panic!("stream should yield a completed response");
    }

    #[tokio::test]
    async fn openai_chat_stream_usage_obeys_profile_precedence_and_decodes_terminal_usage() {
        const OPTED_IN_MODEL: &str = "baseten/usage";
        const OPTED_OUT_MODEL: &str = "baseten/no-usage";

        let server = MockServer::start().await;
        let captured = Arc::new(Mutex::new(Vec::<Value>::new()));
        let captured_for_mock = Arc::clone(&captured);
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(move |request: &wiremock::Request| {
                captured_for_mock
                    .lock()
                    .expect("capture mutex")
                    .push(serde_json::from_slice(&request.body).expect("valid request JSON"));
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer\"},\"finish_reason\":null}]}\n\n\
                         data: {\"id\":\"chatcmpl-baseten\",\"object\":\"chat.completion.chunk\",\"model\":\"baseten/usage\",\"choices\":[],\"usage\":{\"prompt_tokens\":13,\"completion_tokens\":5,\"total_tokens\":18,\"completion_tokens_details\":{\"reasoning_tokens\":3}}}\n\n\
                         data: [DONE]\n\n",
                    )
            })
            .expect(2)
            .mount(&server)
            .await;

        let config = CustomProviderConfig {
            name: "baseten".to_string(),
            display_name: "Baseten".to_string(),
            base_url: server.uri(),
            api_format: CustomProviderApiFormat::OpenAIChat,
            supports_stream_usage: Some(true),
            model: OPTED_IN_MODEL.to_string(),
            models: vec![OPTED_IN_MODEL.to_string(), OPTED_OUT_MODEL.to_string()],
            profiles: std::collections::BTreeMap::from([(
                OPTED_OUT_MODEL.to_string(),
                CustomProviderProfileConfig {
                    supports_stream_usage: Some(false),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let router = CustomProviderBackendRouter::from_config(
            config,
            Some("fixture-key".to_string()),
            Some(OPTED_IN_MODEL.to_string()),
            server.uri(),
            None,
            None,
            None,
            Some(AnthropicConfig::default()),
            None,
            None,
        );

        let response = collect_completed_response(&router, OPTED_IN_MODEL).await;
        let usage = response.usage.expect("terminal Baseten usage should be retained");
        assert_eq!((usage.prompt_tokens, usage.completion_tokens, usage.total_tokens), (13, 5, 18));
        assert_eq!(usage.reasoning_output_tokens, Some(3));
        drop(collect_completed_response(&router, OPTED_OUT_MODEL).await);

        let requests = captured.lock().expect("capture mutex");
        let opted_in = requests
            .iter()
            .find(|request| request["model"] == OPTED_IN_MODEL)
            .expect("opted-in request should be captured");
        let opted_out = requests
            .iter()
            .find(|request| request["model"] == OPTED_OUT_MODEL)
            .expect("opted-out request should be captured");
        assert_eq!(opted_in["stream_options"]["include_usage"], true);
        assert!(opted_out.get("stream_options").is_none());
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
