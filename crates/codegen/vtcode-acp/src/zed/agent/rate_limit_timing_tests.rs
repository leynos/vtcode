//! Deterministic retry scheduling against Tokio's injected clock.

use std::sync::Mutex;

use async_trait::async_trait;
use vtcode_config::core::{CustomProviderConfig, CustomProviderRequestPolicyConfig};
use vtcode_core::llm::provider::LLMErrorMetadata;

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
