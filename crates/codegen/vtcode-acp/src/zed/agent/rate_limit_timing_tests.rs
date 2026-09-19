//! Deterministic retry scheduling against Tokio's injected clock.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use async_trait::async_trait;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use vtcode_core::llm::provider::LLMErrorMetadata;

use super::tests::{PROMPT_PROVIDER_TEST_LOCK, PromptProviderFactoryGuard, build_wire_test_agent_with_providers};
use super::*;
use crate::zed::{provider_runtime::ProviderRuntimeRegistry, types::SessionCancellation};

#[derive(Default)]
struct RateLimitedProvider {
    attempts: Mutex<Vec<Instant>>,
}

#[async_trait]
impl LLMProvider for RateLimitedProvider {
    fn name(&self) -> &str {
        "timing"
    }
    fn supported_models(&self) -> Vec<String> {
        vec!["fixture".to_string()]
    }
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let attempt = {
            let mut attempts = self.attempts.lock().expect("record attempt");
            let attempt = attempts.len();
            attempts.push(Instant::now());
            attempt
        };
        if attempt < 2 {
            return Err(LLMError::RateLimit {
                metadata: Some(LLMErrorMetadata::new(
                    "timing",
                    Some(429),
                    None,
                    None,
                    None,
                    (attempt == 0).then(|| "15".to_string()),
                    None,
                )),
            });
        }
        Ok(LLMResponse::new("fixture", "recovered"))
    }
}

#[tokio::test(start_paused = true)]
async fn buffered_acp_retries_remember_server_floor_with_injected_time() {
    let provider = RateLimitedProvider::default();
    let config = CustomProviderConfig {
        name: "timing".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 10_000,
            retry_max_backoff_ms: 10_000,
            retry_jitter: false,
            ..Default::default()
        },
        ..Default::default()
    };
    let runtime =
        ProviderRuntimeRegistry::new(&[config], &vtcode_config::TimeoutsConfig::default()).for_provider("timing");
    let response =
        generate_with_retry(&provider, LLMRequest::default(), &runtime, &SessionCancellation::default(), None)
            .await
            .expect("retry should recover");
    assert_eq!(response.content.as_deref(), Some("recovered"));
    let attempts = provider.attempts.lock().expect("attempt times");
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts[1] - attempts[0], Duration::from_secs(15));
    assert_eq!(attempts[2] - attempts[1], Duration::from_secs(30));
    assert_eq!(runtime.telemetry_snapshot().active_permits, 0);
}

#[tokio::test(start_paused = true)]
async fn unrepresentable_backoff_remains_cancellable_without_timer_overflow() {
    let cancellation = SessionCancellation::default();
    let cancel = async {
        tokio::time::sleep(Duration::from_secs(1)).await;
        cancellation.cancel();
    };
    let (result, ()) = tokio::join!(cancellable_backoff(Duration::MAX, &cancellation), cancel);
    assert!(matches!(result, Err(ProviderCallError::Cancelled)));
}

const FIXED_OBSERVATION_SECONDS: u64 = 1_445_412_420;
const FIXED_RETRY_AFTER_DATE: &str = "Wed, 21 Oct 2015 07:27:01 GMT";

fn fixed_observation_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(FIXED_OBSERVATION_SECONDS)
}

fn http_date_rate_limit_error(attempt_index: usize) -> LLMError {
    let retry_after = (attempt_index == 0).then(|| FIXED_RETRY_AFTER_DATE.to_string());
    LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "wire-test",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            retry_after,
            Some("fixture rate limit".to_string()),
        )),
    }
}

struct FixedObservationBufferedProvider {
    attempts: Mutex<Vec<Instant>>,
}

#[async_trait]
impl LLMProvider for FixedObservationBufferedProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let attempt_index = {
            let mut attempts = self.attempts.lock().expect("record buffered retry attempt");
            let attempt_index = attempts.len();
            attempts.push(Instant::now());
            attempt_index
        };
        if attempt_index < 2 {
            return Err(http_date_rate_limit_error(attempt_index));
        }
        Ok(LLMResponse::new("wire-model", "buffered recovered"))
    }
}

struct FixedObservationStreamProvider {
    attempts: Arc<Mutex<Vec<Instant>>>,
    next_attempt: AtomicUsize,
}

#[async_trait]
impl LLMProvider for FixedObservationStreamProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn supports_tools(&self, _model: &str) -> bool {
        false
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Err(LLMError::InvalidRequest {
            message: "the streaming fixture must use the streaming ACP path".to_string(),
            metadata: None,
        })
    }

    async fn stream(&self, _request: LLMRequest) -> Result<vtcode_core::llm::provider::LLMStream, LLMError> {
        let attempt_index = self.next_attempt.fetch_add(1, Ordering::SeqCst);
        self.attempts
            .lock()
            .expect("record streaming retry attempt")
            .push(Instant::now());
        if attempt_index < 2 {
            return Err(http_date_rate_limit_error(attempt_index));
        }

        let response = LLMResponse::new("wire-model", "stream recovered");
        Ok(Box::pin(futures::stream::iter([Ok(LLMStreamEvent::Completed { response: Box::new(response) })])))
    }
}

fn fixed_observation_provider_config() -> CustomProviderConfig {
    CustomProviderConfig {
        name: "wire-test".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_retries: 2,
            retry_initial_backoff_ms: 10,
            retry_max_backoff_ms: 100,
            retry_jitter: false,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn fixed_observation_runtime() -> ProviderRequestRuntime {
    ProviderRuntimeRegistry::new(&[fixed_observation_provider_config()], &vtcode_config::TimeoutsConfig::default())
        .for_provider("wire-test")
}

fn assert_http_date_then_local_backoff(attempts: &[Instant]) {
    assert_eq!(attempts.len(), 3);
    assert_eq!(attempts[1] - attempts[0], Duration::from_secs(1));
    assert_eq!(attempts[2] - attempts[1], Duration::from_millis(20));
}

#[tokio::test(start_paused = true)]
async fn buffered_acp_retry_uses_fixed_http_date_observation_and_drops_it_afterward() {
    let provider = FixedObservationBufferedProvider { attempts: Mutex::new(Vec::new()) };
    let response = generate_with_retry_with_observer(
        &provider,
        LLMRequest::default(),
        &fixed_observation_runtime(),
        &SessionCancellation::default(),
        None,
        &fixed_observation_time,
    )
    .await
    .expect("buffered fixed-observation retry should recover");

    assert_eq!(response.content.as_deref(), Some("buffered recovered"));
    let attempts = provider.attempts.lock().expect("buffered retry attempt times");
    assert_http_date_then_local_backoff(&attempts);
}

#[tokio::test(start_paused = true)]
async fn streaming_acp_retry_uses_fixed_http_date_observation_and_drops_it_afterward() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let attempts_for_factory = Arc::clone(&attempts);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || {
            Box::new(FixedObservationStreamProvider {
                attempts: Arc::clone(&attempts_for_factory),
                next_attempt: AtomicUsize::new(0),
            })
        }),
    );
    let workspace = assert_fs::TempDir::new().expect("streaming retry workspace");
    let agent =
        Arc::new(build_wire_test_agent_with_providers(workspace.path(), &[fixed_observation_provider_config()]).await);
    let session_id = agent.register_session();

    let response = run_prompt_with_retry_observer(
        agent,
        PromptRequest::new(session_id, vec![acp::ContentBlock::Text(acp::TextContent::new("retry the stream"))]),
        fixed_observation_time,
    )
    .await
    .expect("streaming fixed-observation retry should recover");

    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    let attempts = attempts.lock().expect("streaming retry attempt times");
    assert_http_date_then_local_backoff(&attempts);
}
