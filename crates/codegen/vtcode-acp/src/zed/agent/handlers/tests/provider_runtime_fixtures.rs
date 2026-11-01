//! Shared mock providers and tracing capture for provider-runtime handler tests.
use super::*;

#[derive(Clone, Default)]
pub(crate) struct TraceCapture(pub(crate) Arc<Mutex<Vec<String>>>);

impl<S> Layer<S> for TraceCapture
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(&self, event: &tracing::Event<'_>, context: Context<'_, S>) {
        let current = context.lookup_current().map(|span| span.name().to_owned());
        let mut fields = CapturedFields::default();
        event.record(&mut fields);
        let mut line = format!("span={current:?} event={}", event.metadata().name());
        line.push_str(&fields.0);
        self.0.lock().expect("trace capture lock").push(line);
    }

    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, _id: &tracing::Id, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        attrs.record(&mut fields);
        let mut line = format!("span={}", attrs.metadata().name());
        line.push_str(&fields.0);
        self.0.lock().expect("trace capture lock").push(line);
    }

    fn on_record(&self, _id: &tracing::Id, values: &tracing::span::Record<'_>, _context: Context<'_, S>) {
        let mut fields = CapturedFields::default();
        values.record(&mut fields);
        self.0.lock().expect("trace capture lock").push(fields.0);
    }
}

#[derive(Default)]
struct CapturedFields(String);

impl Visit for CapturedFields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        write!(&mut self.0, " {}={value:?}", field.name()).expect("write trace field");
    }
}

pub(super) struct FlakyProvider {
    pub(super) attempts: AtomicUsize,
    pub(super) network_failures: usize,
    pub(super) invalid_request: bool,
}

pub(super) struct PendingProvider;

pub(super) struct BadGatewayProvider {
    pub(super) attempts: AtomicUsize,
}

pub(super) struct RateLimitedProvider {
    pub(super) attempts: AtomicUsize,
}

#[async_trait]
impl LLMProvider for PendingProvider {
    fn name(&self) -> &str {
        "timeout-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        std::future::pending().await
    }
}

#[async_trait]
impl LLMProvider for BadGatewayProvider {
    fn name(&self) -> &str {
        "bad-gateway-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        false
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let _previous_attempts = self.attempts.fetch_add(1, Ordering::SeqCst);
        Err(LLMError::Provider {
            message: "OpenAI Chat Completions error (status 502 Bad Gateway)".to_string(),
            metadata: Some(LLMErrorMetadata::new(
                "openai",
                Some(502),
                None,
                None,
                None,
                None,
                Some("502 Bad Gateway".to_string()),
            )),
        })
    }
}

#[async_trait]
impl LLMProvider for RateLimitedProvider {
    fn name(&self) -> &str {
        "retry-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return Err(LLMError::RateLimit {
                metadata: Some(LLMErrorMetadata::new(
                    "baseten",
                    Some(429),
                    Some("rate_limit_error".to_string()),
                    None,
                    None,
                    Some("0.2".to_string()),
                    Some("capacity temporarily exhausted".to_string()),
                )),
            });
        }
        Ok(LLMResponse::new("test-model", "recovered"))
    }
}

#[async_trait]
impl LLMProvider for FlakyProvider {
    fn name(&self) -> &str {
        "retry-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["test-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
        if self.invalid_request {
            return Err(LLMError::InvalidRequest { message: "bad request".to_string(), metadata: None });
        }
        if attempt < self.network_failures {
            return Err(LLMError::Network {
                message: "temporary disconnect".to_string(),
                metadata: None,
            });
        }
        Ok(LLMResponse::new("test-model", "recovered"))
    }
}

pub(crate) fn retry_runtime() -> ProviderRequestRuntime {
    let provider = CustomProviderConfig {
        name: "retry-test".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 1,
            retry_max_backoff_ms: 2,
            retry_jitter: false,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    crate::zed::provider_runtime::ProviderRuntimeRegistry::new(
        &[provider],
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider("retry-test")
}

pub(super) fn timeout_runtime() -> ProviderRequestRuntime {
    let provider = CustomProviderConfig {
        name: "timeout-test".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 0,
            total_generation_timeout_seconds: 1,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    crate::zed::provider_runtime::ProviderRuntimeRegistry::new(
        &[provider],
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider("timeout-test")
}
