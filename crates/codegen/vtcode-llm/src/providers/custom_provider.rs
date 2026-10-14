//! Routes configured custom-provider requests and enforces their concurrency, retry, and stream policies.
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, Weak};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use vtcode_commons::llm::{BackendKind, LLMErrorMetadata};
use vtcode_config::core::AnthropicConfig;
use vtcode_config::core::{CustomProviderApiFormat, CustomProviderConfig, ResolvedCustomProviderProfile};
use vtcode_config::{ModelConfig, OpenAIConfig, PromptCachingConfig, TimeoutsConfig};

use crate::provider::{
    LLMError, LLMNormalizedStream, LLMProvider, LLMRequest, LLMResponse, LLMStream, Message,
    ResponsesCompactionOptions, SamplingOverrides,
};
use crate::providers::anthropic::{self, AnthropicProvider};
use crate::providers::common::{resolve_model, validate_request_common};
use crate::providers::openai::{CustomProviderAuthHandle, OpenAIProvider};

static SHARED_PROVIDER_SEMAPHORES: LazyLock<Mutex<HashMap<String, Weak<Semaphore>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static ADMISSION_METRICS: LazyLock<AdmissionMetricCounters> = LazyLock::new(AdmissionMetricCounters::default);

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct CustomProviderAdmissionMetrics {
    pub queue_depth: u64,
    pub in_flight: u64,
    pub queue_waits: u64,
    pub queue_wait_ms: u64,
    pub queue_timeouts: u64,
}

#[derive(Default)]
struct AdmissionMetricCounters {
    queue_depth: AtomicU64,
    in_flight: AtomicU64,
    queue_waits: AtomicU64,
    queue_wait_ms: AtomicU64,
    queue_timeouts: AtomicU64,
}

fn saturating_add(counter: &AtomicU64, amount: u64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_add(amount)));
}

fn saturating_sub_one(counter: &AtomicU64) {
    let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| Some(value.saturating_sub(1)));
}

/// Samples the process-wide aggregate custom-provider admission counters.
///
/// `queue_depth` and `in_flight` are current counts; the remaining fields are
/// cumulative queue waits, wait time in milliseconds and queue timeouts. Each
/// field is loaded independently with relaxed ordering, so concurrent updates
/// can make the values reflect slightly different instants. Reading the
/// snapshot does not mutate or reset the counters.
pub fn custom_provider_admission_metrics() -> CustomProviderAdmissionMetrics {
    CustomProviderAdmissionMetrics {
        queue_depth: ADMISSION_METRICS.queue_depth.load(Ordering::Relaxed),
        in_flight: ADMISSION_METRICS.in_flight.load(Ordering::Relaxed),
        queue_waits: ADMISSION_METRICS.queue_waits.load(Ordering::Relaxed),
        queue_wait_ms: ADMISSION_METRICS.queue_wait_ms.load(Ordering::Relaxed),
        queue_timeouts: ADMISSION_METRICS.queue_timeouts.load(Ordering::Relaxed),
    }
}

struct AdmissionQueueWait {
    provider_name: Arc<str>,
    started_at: Instant,
    timed_out: bool,
}

impl AdmissionQueueWait {
    fn start(provider_name: Arc<str>) -> Self {
        saturating_add(&ADMISSION_METRICS.queue_depth, 1);
        Self {
            provider_name,
            started_at: Instant::now(),
            timed_out: false,
        }
    }
}

impl Drop for AdmissionQueueWait {
    fn drop(&mut self) {
        let waited = self.started_at.elapsed();
        let waited_ms = u64::try_from(waited.as_millis()).unwrap_or(u64::MAX);
        saturating_sub_one(&ADMISSION_METRICS.queue_depth);
        saturating_add(&ADMISSION_METRICS.queue_waits, 1);
        saturating_add(&ADMISSION_METRICS.queue_wait_ms, waited_ms);
        if self.timed_out {
            saturating_add(&ADMISSION_METRICS.queue_timeouts, 1);
            tracing::warn!(
                provider = self.provider_name.as_ref(),
                queue_wait_ms = waited_ms,
                "Custom provider request admission timed out"
            );
        } else {
            tracing::debug!(
                provider = self.provider_name.as_ref(),
                queue_wait_ms = waited_ms,
                "Custom provider request admitted"
            );
        }
    }
}

#[derive(Debug)]
struct CustomProviderAdmissionPermit {
    _permit: OwnedSemaphorePermit,
}

impl CustomProviderAdmissionPermit {
    fn new(permit: OwnedSemaphorePermit) -> Self {
        saturating_add(&ADMISSION_METRICS.in_flight, 1);
        Self { _permit: permit }
    }
}

impl Drop for CustomProviderAdmissionPermit {
    fn drop(&mut self) {
        saturating_sub_one(&ADMISSION_METRICS.in_flight);
    }
}

#[derive(Clone)]
struct CustomProviderAdmission {
    provider_name: Arc<str>,
    semaphore: Option<Arc<Semaphore>>,
    queue_timeout: Duration,
}

impl CustomProviderAdmission {
    fn new(config: &CustomProviderConfig, effective_base_url: &str) -> Self {
        let provider_name: Arc<str> = Arc::from(config.name.as_str());
        let semaphore = config.request_policy.max_in_flight_requests.map(|limit| {
            let identity = serde_json::to_vec(&(config, effective_base_url)).ok();
            match identity {
                Some(identity) => {
                    let key = vtcode_commons::utils::calculate_sha256(&identity);
                    let mut shared = SHARED_PROVIDER_SEMAPHORES
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    shared.retain(|_, semaphore| semaphore.strong_count() > 0);
                    if let Some(semaphore) = shared.get(&key).and_then(Weak::upgrade) {
                        semaphore
                    } else {
                        let semaphore = Arc::new(Semaphore::new(limit));
                        shared.insert(key, Arc::downgrade(&semaphore));
                        semaphore
                    }
                }
                None => {
                    tracing::warn!(
                        provider = %provider_name,
                        "Unable to identify custom provider configuration for shared request admission; using an isolated gate"
                    );
                    Arc::new(Semaphore::new(limit))
                }
            }
        });

        Self {
            provider_name,
            semaphore,
            queue_timeout: Duration::from_secs(config.request_policy.queue_timeout_seconds),
        }
    }

    async fn acquire(&self) -> Result<Option<CustomProviderAdmissionPermit>, LLMError> {
        let Some(semaphore) = &self.semaphore else {
            return Ok(None);
        };
        let mut queue_wait = AdmissionQueueWait::start(Arc::clone(&self.provider_name));
        match tokio::time::timeout(self.queue_timeout, Arc::clone(semaphore).acquire_owned()).await {
            Ok(Ok(permit)) => {
                drop(queue_wait);
                Ok(Some(CustomProviderAdmissionPermit::new(permit)))
            }
            Ok(Err(_closed)) => Err(LLMError::Provider {
                message: format!("custom provider '{}' request admission is closed", self.provider_name),
                metadata: Some(LLMErrorMetadata::new(
                    self.provider_name.as_ref(),
                    None,
                    Some("custom_provider_admission_closed".to_owned()),
                    None,
                    None,
                    None,
                    None,
                )),
            }),
            Err(_elapsed) => {
                queue_wait.timed_out = true;
                Err(LLMError::Provider {
                    message: format!(
                        "custom provider '{}' remained at its configured request limit for {:?}",
                        self.provider_name, self.queue_timeout,
                    ),
                    metadata: Some(LLMErrorMetadata::new(
                        self.provider_name.as_ref(),
                        None,
                        Some("custom_provider_admission_timeout".to_owned()),
                        None,
                        None,
                        None,
                        None,
                    )),
                })
            }
        }
    }
}

/// Profile-aware backend router for custom providers.
pub struct CustomProviderBackendRouter {
    provider_key: String,
    display_name: String,
    default_model: String,
    supported_models: Vec<String>,
    custom_config: CustomProviderConfig,
    request_admission: CustomProviderAdmission,
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
        model_behaviour: Option<ModelConfig>,
        custom_provider_auth: Option<CustomProviderAuthHandle>,
    ) -> Self {
        let provider_key = custom_config.name.clone();
        let request_admission = CustomProviderAdmission::new(&custom_config, &base_url);
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
            model_behaviour.clone(),
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
            model_behaviour.clone(),
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
            model_behaviour.clone(),
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
            model_behaviour,
        )
        .with_custom_auth(custom_provider_auth);

        Self {
            provider_key,
            display_name,
            default_model,
            supported_models,
            custom_config,
            request_admission,
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
        let _permit = self.request_admission.acquire().await?;
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
        let _permit = self.request_admission.acquire().await?;
        self.backend_for_model(model)
            .compact_history_with_options(self.resolved_model(model), history, options)
            .await
    }

    async fn stream(&self, request: LLMRequest) -> Result<LLMStream, LLMError> {
        let permit = self.request_admission.acquire().await?;
        let mut stream = self.backend_for_model(&request.model).stream(request).await?;
        Ok(Box::pin(async_stream::stream! {
            let _permit = permit;
            while let Some(event) = stream.next().await {
                yield event;
            }
        }))
    }

    async fn stream_normalized(&self, request: LLMRequest) -> Result<LLMNormalizedStream, LLMError> {
        let permit = self.request_admission.acquire().await?;
        let mut stream = self.backend_for_model(&request.model).stream_normalized(request).await?;
        Ok(Box::pin(async_stream::stream! {
            let _permit = permit;
            while let Some(event) = stream.next().await {
                yield event;
            }
        }))
    }

    async fn generate(&self, request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let _permit = self.request_admission.acquire().await?;
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
    use super::{CustomProviderAdmission, CustomProviderBackendRouter, custom_provider_admission_metrics};
    use crate::provider::{
        LLMProvider, LLMRequest, LLMStreamEvent, Message, SamplingOverrides, ToolCall, ToolDefinition,
    };
    use crate::providers::openai::CustomProviderAuthHandle;
    use futures::StreamExt;
    use proptest::prelude::*;
    use serde_json::{Value, json};
    use std::future::Future;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::task::Poll;
    use std::time::Duration;
    use tempfile::TempDir;
    use tokio::sync::Semaphore;
    use vtcode_config::constants::models;
    use vtcode_config::core::{
        AnthropicConfig, CustomProviderApiFormat, CustomProviderCommandAuthConfig, CustomProviderConfig,
        CustomProviderProfileConfig, CustomProviderRequestPolicyConfig,
    };
    use vtcode_config::types::ReasoningEffortLevel;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    static ADMISSION_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

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

    fn admission_router(base_url: &str) -> CustomProviderBackendRouter {
        CustomProviderBackendRouter::from_config(
            CustomProviderConfig {
                name: "shared-admission-regression".to_string(),
                display_name: "Shared Admission Regression".to_string(),
                base_url: base_url.to_string(),
                request_policy: CustomProviderRequestPolicyConfig {
                    max_in_flight_requests: Some(1),
                    queue_timeout_seconds: 5,
                    ..CustomProviderRequestPolicyConfig::default()
                },
                ..CustomProviderConfig::default()
            },
            None,
            None,
            base_url.to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
        )
    }

    fn admission_for_capacity(provider_name: &str, capacity: usize) -> CustomProviderAdmission {
        let config = CustomProviderConfig {
            name: provider_name.to_string(),
            request_policy: CustomProviderRequestPolicyConfig {
                max_in_flight_requests: Some(capacity),
                queue_timeout_seconds: 30,
                ..CustomProviderRequestPolicyConfig::default()
            },
            ..CustomProviderConfig::default()
        };

        CustomProviderAdmission::new(&config, &format!("https://{provider_name}.example/v1"))
    }

    #[tokio::test]
    async fn router_instances_share_admission_for_the_same_endpoint_and_isolate_other_endpoints() {
        let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
        let first_router = admission_router("https://same.example/v1");
        let second_router = admission_router("https://same.example/v1");
        let other_endpoint = admission_router("https://other.example/v1");
        let first_permit = first_router
            .request_admission
            .acquire()
            .await
            .expect("first router should acquire the configured permit");

        let second_admission = second_router.request_admission.clone();
        let queued = tokio::spawn(async move { second_admission.acquire().await });
        tokio::task::yield_now().await;
        assert!(!queued.is_finished(), "the second router must share the first router's limit");

        let separate_permit =
            tokio::time::timeout(Duration::from_millis(100), other_endpoint.request_admission.acquire())
                .await
                .expect("a different endpoint should have an independent limit")
                .expect("different endpoint permit");

        drop(first_permit);
        let second_permit = tokio::time::timeout(Duration::from_secs(1), queued)
            .await
            .expect("shared permit should be released")
            .expect("queued task")
            .expect("second router should acquire the released permit");
        drop((second_permit, separate_permit));
    }

    #[tokio::test]
    async fn request_admission_reports_queue_timeout_after_the_configured_wait() {
        let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
        let metrics_before = custom_provider_admission_metrics();
        let admission = CustomProviderAdmission {
            provider_name: Arc::from("queue-timeout-regression"),
            semaphore: Some(Arc::new(Semaphore::new(1))),
            queue_timeout: Duration::from_millis(5),
        };
        let _held = admission.acquire().await.expect("first request permit");

        let error = admission
            .acquire()
            .await
            .expect_err("second request should time out in the queue");

        let crate::provider::LLMError::Provider { message, metadata } = error else {
            panic!("queue timeout should return a provider error");
        };
        assert!(message.contains("configured request limit"));
        assert_eq!(
            metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
            Some("custom_provider_admission_timeout"),
        );
        let metrics_after = custom_provider_admission_metrics();
        assert!(metrics_after.queue_waits > metrics_before.queue_waits);
        assert!(metrics_after.queue_wait_ms > metrics_before.queue_wait_ms);
        assert!(metrics_after.queue_timeouts > metrics_before.queue_timeouts);
        assert!(metrics_after.in_flight >= 1, "the held permit must be included in shared admission metrics");
    }

    #[tokio::test]
    async fn closed_request_admission_has_stable_error_metadata() {
        let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
        let semaphore = Arc::new(Semaphore::new(1));
        semaphore.close();
        let admission = CustomProviderAdmission {
            provider_name: Arc::from("closed-gate-regression"),
            semaphore: Some(semaphore),
            queue_timeout: Duration::from_millis(5),
        };

        let error = admission.acquire().await.expect_err("closed gate should reject admission");

        let crate::provider::LLMError::Provider { metadata, .. } = error else {
            panic!("closed gate should return a provider error");
        };
        assert_eq!(
            metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
            Some("custom_provider_admission_closed"),
        );
    }

    #[tokio::test]
    async fn request_admission_respects_configured_capacities_and_releases_on_completion() {
        let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
        for capacity in [1, 2, 4] {
            let admission = Arc::new(admission_for_capacity(&format!("capacity-{capacity}"), capacity));
            let request_count = capacity * 3;
            let active_requests = Arc::new(AtomicUsize::new(0));
            let peak_active_requests = Arc::new(AtomicUsize::new(0));
            let start_gate = Arc::new(tokio::sync::Barrier::new(request_count + 1));
            let (started_sender, mut started_receiver) = tokio::sync::mpsc::channel(request_count);
            let (admitted_sender, mut admitted_receiver) = tokio::sync::mpsc::channel(request_count);
            let (release_sender, release_receiver) = tokio::sync::watch::channel(false);
            let mut requests = tokio::task::JoinSet::new();

            for request_id in 0..request_count {
                let admission = Arc::clone(&admission);
                let active_requests = Arc::clone(&active_requests);
                let peak_active_requests = Arc::clone(&peak_active_requests);
                let start_gate = Arc::clone(&start_gate);
                let started_sender = started_sender.clone();
                let admitted_sender = admitted_sender.clone();
                let mut release_receiver = release_receiver.clone();

                requests.spawn(async move {
                    start_gate.wait().await;
                    started_sender
                        .send(request_id)
                        .await
                        .expect("test should receive every started request");

                    let permit = admission
                        .acquire()
                        .await
                        .expect("request should be admitted")
                        .expect("configured capacity should return a permit");
                    let active = active_requests.fetch_add(1, Ordering::SeqCst) + 1;
                    peak_active_requests.fetch_max(active, Ordering::SeqCst);
                    admitted_sender
                        .send(request_id)
                        .await
                        .expect("test should receive every admitted request");

                    release_receiver
                        .wait_for(|released| *released)
                        .await
                        .expect("test should release every admitted request");
                    active_requests.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                });
            }

            start_gate.wait().await;
            for _ in 0..request_count {
                tokio::time::timeout(Duration::from_secs(1), started_receiver.recv())
                    .await
                    .expect("all concurrent requests should start")
                    .expect("started request channel should remain open");
            }

            let mut admitted_request_ids = Vec::with_capacity(capacity);
            for _ in 0..capacity {
                admitted_request_ids.push(
                    tokio::time::timeout(Duration::from_secs(2), admitted_receiver.recv())
                        .await
                        .expect("configured capacity should be admitted")
                        .expect("admitted request channel should remain open"),
                );
            }
            admitted_request_ids.sort_unstable();
            admitted_request_ids.dedup();
            assert_eq!(admitted_request_ids.len(), capacity, "each permit admits one request");
            assert_eq!(active_requests.load(Ordering::SeqCst), capacity);
            assert_eq!(peak_active_requests.load(Ordering::SeqCst), capacity);
            assert!(
                tokio::time::timeout(Duration::from_millis(50), admitted_receiver.recv())
                    .await
                    .is_err(),
                "no request may be admitted while all configured permits are held"
            );

            release_sender.send(true).expect("request workers are still listening");
            while let Some(result) = requests.join_next().await {
                result.expect("admission worker should complete without panicking");
            }

            assert_eq!(active_requests.load(Ordering::SeqCst), 0);
            assert_eq!(peak_active_requests.load(Ordering::SeqCst), capacity);
            assert_eq!(
                admission.semaphore.as_ref().expect("configured semaphore").available_permits(),
                capacity,
                "completed requests should return every permit"
            );

            let mut reused_permits = Vec::with_capacity(capacity);
            for _ in 0..capacity {
                reused_permits.push(
                    tokio::time::timeout(Duration::from_secs(1), admission.acquire())
                        .await
                        .expect("released permit should be reusable")
                        .expect("reused request should be admitted")
                        .expect("configured capacity should return a permit"),
                );
            }
            assert_eq!(admission.semaphore.as_ref().expect("configured semaphore").available_permits(), 0);
            drop(reused_permits);
        }
    }

    #[tokio::test]
    async fn dropping_a_queued_admission_future_does_not_leak_a_permit() {
        let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
        let admission = admission_for_capacity("cancelled-queued-request", 1);
        let held_permit = admission
            .acquire()
            .await
            .expect("first request should be admitted")
            .expect("configured capacity should return a permit");
        let mut queued_acquisition = Box::pin(admission.acquire());
        let poll_result =
            futures::future::poll_fn(|context| Poll::Ready(queued_acquisition.as_mut().poll(context))).await;

        assert!(poll_result.is_pending(), "acquisition should queue behind the held permit");
        drop(queued_acquisition);
        drop(held_permit);

        let reacquired = tokio::time::timeout(Duration::from_secs(1), admission.acquire())
            .await
            .expect("cancelled waiter should not retain the released permit")
            .expect("permit should remain available after cancellation")
            .expect("configured capacity should return a permit");
        assert_eq!(admission.semaphore.as_ref().expect("configured semaphore").available_permits(), 0);
        drop(reacquired);
        assert_eq!(admission.semaphore.as_ref().expect("configured semaphore").available_permits(), 1);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        #[test]
        fn generated_admission_operations_preserve_permit_and_queue_invariants(
            capacity in 1_usize..=4,
            operations in prop::collection::vec((0_u8..4, any::<usize>()), 1..32),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build admission model runtime");
            runtime.block_on(async {
                let _admission_test_guard = ADMISSION_TEST_LOCK.lock().await;
                let provider_name = format!("model-{capacity}");
                let mut admission = admission_for_capacity(&provider_name, capacity);
                admission.queue_timeout = Duration::from_millis(1);
                let metrics_before = custom_provider_admission_metrics();
                let mut held_permits = Vec::with_capacity(capacity);
                let mut expected_waits = 0_u64;
                let mut expected_timeouts = 0_u64;

                for (operation, index) in operations {
                    match operation {
                        0 => {
                            expected_waits += 1;
                            if held_permits.len() < capacity {
                                held_permits.push(
                                    admission
                                        .acquire()
                                        .await
                                        .expect("available modeled capacity should admit")
                                        .expect("configured capacity should return a permit"),
                                );
                            } else {
                                expected_timeouts += 1;
                                let error = admission.acquire().await.expect_err("full capacity should time out");
                                let crate::provider::LLMError::Provider { metadata, .. } = error else {
                                    panic!("full capacity should return a provider error");
                                };
                                prop_assert_eq!(
                                    metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
                                    Some("custom_provider_admission_timeout"),
                                );
                            }
                        }
                        1 if !held_permits.is_empty() => {
                            let release_index = index % held_permits.len();
                            drop(held_permits.swap_remove(release_index));
                        }
                        2 if held_permits.len() == capacity => {
                            expected_waits += 1;
                            let mut queued_acquisition = Box::pin(admission.acquire());
                            let poll_result = futures::future::poll_fn(|context| {
                                Poll::Ready(queued_acquisition.as_mut().poll(context))
                            })
                            .await;
                            prop_assert!(poll_result.is_pending(), "a full gate must queue acquisition");
                            prop_assert_eq!(
                                custom_provider_admission_metrics().queue_depth,
                                metrics_before.queue_depth + 1,
                            );
                            drop(queued_acquisition);
                            prop_assert_eq!(custom_provider_admission_metrics().queue_depth, metrics_before.queue_depth);
                        }
                        3 if held_permits.len() == capacity => {
                            expected_waits += 1;
                            expected_timeouts += 1;
                            let error = admission.acquire().await.expect_err("full capacity should time out");
                            let crate::provider::LLMError::Provider { metadata, .. } = error else {
                                panic!("full capacity should return a provider error");
                            };
                            prop_assert_eq!(
                                metadata.as_deref().and_then(|metadata| metadata.code.as_deref()),
                                Some("custom_provider_admission_timeout"),
                            );
                        }
                        _ => {}
                    }

                    prop_assert!(held_permits.len() <= capacity, "active permits must stay within capacity");
                    prop_assert_eq!(
                        admission.semaphore.as_ref().expect("configured semaphore").available_permits()
                            + held_permits.len(),
                        capacity,
                        "every permit is either held or available",
                    );
                    prop_assert_eq!(custom_provider_admission_metrics().queue_depth, metrics_before.queue_depth);
                    prop_assert_eq!(
                        custom_provider_admission_metrics().in_flight,
                        metrics_before.in_flight + u64::try_from(held_permits.len()).expect("held count fits u64"),
                    );
                }

                drop(held_permits);
                prop_assert_eq!(
                    admission.semaphore.as_ref().expect("configured semaphore").available_permits(),
                    capacity,
                    "dropping every active request must return all permits",
                );
                let metrics_after = custom_provider_admission_metrics();
                prop_assert_eq!(metrics_after.queue_depth, metrics_before.queue_depth);
                prop_assert_eq!(metrics_after.in_flight, metrics_before.in_flight);
                prop_assert_eq!(metrics_after.queue_waits - metrics_before.queue_waits, expected_waits);
                prop_assert_eq!(metrics_after.queue_timeouts - metrics_before.queue_timeouts, expected_timeouts);
                Ok(())
            })?;
        }
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
