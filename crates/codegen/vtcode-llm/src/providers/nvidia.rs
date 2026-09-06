//! NVIDIA NIM OpenAI-compatible provider.

use serde_json::{Map, Value};
use vtcode_config::constants::{env_vars, models, urls};
use vtcode_config::types::ReasoningEffortLevel;

use super::extract_reasoning_trace;
use super::openai_compat::{OpenAiCompatCore, OpenAiCompatSpec, impl_openai_compat_provider};
use crate::provider::{LLMError, LLMRequest};

pub struct NvidiaSpec;

fn nvidia_reasoning(message: &Value, choice: &Value) -> Option<String> {
    message
        .get("reasoning_content")
        .and_then(extract_reasoning_trace)
        .or_else(|| choice.get("reasoning_content").and_then(extract_reasoning_trace))
}

impl OpenAiCompatSpec for NvidiaSpec {
    const NAME: &'static str = "NVIDIA";
    const KEY: &'static str = "nvidia";
    const API_KEY_ENV: &'static str = "NVIDIA_API_KEY";
    const DEFAULT_MODEL: &'static str = models::nvidia::DEFAULT_MODEL;
    const DEFAULT_BASE_URL: &'static str = urls::NVIDIA_API_BASE;
    const BASE_URL_ENV: Option<&'static str> = Some(env_vars::NVIDIA_BASE_URL);
    const LISTED_MODELS: &'static [&'static str] = models::nvidia::SUPPORTED_MODELS;
    // NVIDIA exposes a larger catalogue than the curated VT Code picker. An
    // explicit model selection must pass through without local rejection.
    const VALIDATION_ALLOWLIST: Option<&'static [&'static str]> = None;
    const STREAM_OPTIONS_INCLUDE_USAGE: bool = true;
    const RESPONSE_REASONING_EXTRACTOR: Option<super::openai_compat::ReasoningExtractor> = Some(nvidia_reasoning);
    const SUPPRESS_SAMPLING_WHEN_REASONING: bool = false;

    fn resolve_api_key(api_key: Option<String>) -> String {
        api_key
            .or_else(|| std::env::var(Self::API_KEY_ENV).ok().filter(|key| !key.trim().is_empty()))
            .unwrap_or_default()
    }

    fn insert_reasoning(
        _core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        let enable_thinking = request
            .reasoning_effort
            .is_some_and(|effort| effort != ReasoningEffortLevel::None);
        payload.insert("chat_template_kwargs".to_owned(), serde_json::json!({"enable_thinking": enable_thinking}));
        Ok(())
    }

    fn finish_payload(
        _core: &OpenAiCompatCore<Self>,
        request: &LLMRequest,
        payload: &mut Map<String, Value>,
    ) -> Result<(), LLMError> {
        if request.tools.as_ref().is_some_and(|tools| !tools.is_empty())
            && let Some(kwargs) = payload.get_mut("chat_template_kwargs").and_then(Value::as_object_mut)
        {
            kwargs.insert("force_nonempty_content".to_owned(), Value::Bool(true));
        }
        Ok(())
    }
}

impl_openai_compat_provider!(NvidiaProvider, NvidiaSpec, {
    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_structured_output(&self, _model: &str) -> bool {
        true
    }

    fn supports_reasoning(&self, model: &str) -> bool {
        self.core
            .model_behaviour
            .as_ref()
            .and_then(|behaviour| behaviour.model_supports_reasoning)
            .unwrap_or_else(|| models::nvidia::REASONING_MODELS.contains(&model) || !model.trim().is_empty())
    }

    fn supports_reasoning_effort(&self, _model: &str) -> bool {
        true
    }

    fn effective_context_size(&self, _model: &str) -> usize {
        1_000_000
    }
});

#[cfg(test)]
mod tests {
    use super::{NvidiaProvider, NvidiaSpec};
    use crate::BackendKind;
    use crate::provider::{LLMProvider, LLMRequest, LLMStreamEvent, Message, ToolDefinition};
    use crate::providers::common::parse_response_openai_format;
    use crate::providers::openai_compat::OpenAiCompatSpec;
    use crate::providers::shared::{OpenAiDeltaOrder, StreamAggregator, handle_openai_compatible_chunk};
    use serde_json::json;
    use std::sync::Arc;
    use vtcode_config::constants::{models, urls};
    use vtcode_config::types::ReasoningEffortLevel;

    fn provider() -> NvidiaProvider {
        NvidiaProvider::from_config(
            Some("test-key".to_string()),
            Some(models::nvidia::DEFAULT_MODEL.to_string()),
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn base_request() -> LLMRequest {
        LLMRequest {
            messages: vec![Message::user("hello".to_string())].into(),
            model: models::nvidia::DEFAULT_MODEL.to_string(),
            max_tokens: Some(512),
            temperature: Some(1.0),
            top_p: Some(0.95),
            stream: true,
            ..Default::default()
        }
    }

    #[test]
    fn default_config_uses_nvidia_endpoint_and_bearer_key_identity() {
        let provider = provider();
        assert_eq!(provider.core.base_url, urls::NVIDIA_API_BASE);
        assert_eq!(provider.core.api_key, "test-key");
        assert_eq!(NvidiaSpec::API_KEY_ENV, "NVIDIA_API_KEY");
        assert_eq!(provider.backend_kind(), BackendKind::Nvidia);

        let overridden = NvidiaProvider::from_config(
            Some("test-key".to_string()),
            Some(models::nvidia::DEFAULT_MODEL.to_string()),
            Some("https://nvidia-proxy.example/v1".to_string()),
            None,
            None,
            None,
            None,
        );
        assert_eq!(overridden.core.base_url, "https://nvidia-proxy.example/v1");
    }

    #[test]
    fn golden_payload_includes_stream_usage_and_thinking_disabled_by_default() {
        let payload = provider()
            .core
            .convert_request(&base_request())
            .expect("payload should be valid");

        assert_eq!(payload["model"], models::nvidia::DEFAULT_MODEL);
        assert_eq!(payload["stream"], true);
        assert_eq!(payload["stream_options"]["include_usage"], true);
        assert_eq!(payload["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(payload["temperature"], 1.0);
        let top_p = payload["top_p"].as_f64().expect("top_p should be numeric");
        assert!((top_p - 0.95).abs() < 1e-6);
    }

    #[test]
    fn reasoning_effort_toggles_nvidia_thinking() {
        let provider = provider();

        let mut request = base_request();
        request.reasoning_effort = Some(ReasoningEffortLevel::Low);
        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload["chat_template_kwargs"]["enable_thinking"], true);

        request.reasoning_effort = Some(ReasoningEffortLevel::None);
        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload["chat_template_kwargs"]["enable_thinking"], false);
    }

    #[test]
    fn tools_force_nonempty_content_in_chat_template_kwargs() {
        let provider = provider();
        let mut request = base_request();
        request.tools = Some(Arc::new(vec![ToolDefinition::function(
            "get_weather".to_string(),
            "Get weather".to_string(),
            json!({"type": "object", "properties": {"city": {"type": "string"}}}),
        )]));

        let payload = provider.core.convert_request(&request).expect("payload should be valid");
        assert_eq!(payload["chat_template_kwargs"]["force_nonempty_content"], true);
        assert_eq!(payload["tools"][0]["type"], "function");
    }

    #[test]
    fn arbitrary_explicit_nvidia_models_are_not_rejected() {
        let provider = provider();
        let request = LLMRequest {
            model: "nvidia/custom-agent-model".to_string(),
            messages: vec![Message::user("hello".to_string())].into(),
            ..Default::default()
        };

        provider
            .validate_request(&request)
            .expect("NVIDIA should accept explicit catalogue models");
    }

    #[test]
    fn non_streaming_reasoning_content_is_extracted() {
        let response = parse_response_openai_format::<fn(&serde_json::Value, &serde_json::Value) -> Option<String>>(
            json!({
                "choices": [{
                    "message": {
                        "content": "answer",
                        "reasoning_content": "think first"
                    },
                    "finish_reason": "stop"
                }]
            }),
            "NVIDIA",
            models::nvidia::DEFAULT_MODEL.to_string(),
            false,
            Some(super::nvidia_reasoning),
        )
        .expect("response should parse");

        assert_eq!(response.content.as_deref(), Some("answer"));
        assert_eq!(response.reasoning.as_deref(), Some("think first"));
    }

    #[test]
    fn streaming_reasoning_content_is_extracted() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut aggregator = StreamAggregator::new(models::nvidia::DEFAULT_MODEL.to_string());
        let chunk = json!({"choices": [{"delta": {"reasoning_content": "think"}}]});

        handle_openai_compatible_chunk(
            &chunk,
            &mut aggregator,
            &tx,
            NvidiaSpec::STREAM_REASONING_FIELDS,
            OpenAiDeltaOrder::ReasoningFirst,
            false,
        );

        match rx
            .try_recv()
            .expect("reasoning event expected")
            .expect("stream event should be valid")
        {
            LLMStreamEvent::Reasoning { delta } => assert_eq!(delta, "think"),
            other => panic!("expected reasoning event, got {other:?}"),
        }
    }
}
