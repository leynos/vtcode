//! Runtime-scoped observation of private provider failures and concurrent isolation coverage.

use super::super::provider::{ProviderCallError, generate_with_retry};
use super::*;
use crate::zed::{provider_runtime::ProviderRuntimeRegistry, types::SessionCancellation};

#[derive(Clone)]
struct ProviderFailureCapture {
    runtime: ProviderRequestRuntime,
    failures: Arc<Mutex<Vec<String>>>,
}

static PROVIDER_FAILURE_CAPTURE: Mutex<Option<ProviderFailureCapture>> = Mutex::new(None);

/// Observe the same failure consumed by the ACP loop before its intentionally
/// bounded public projection. The provider test lock scopes installation;
/// runtime identity excludes unrelated generations that do not take that lock.
pub(super) struct ProviderFailureCaptureGuard {
    previous: Option<ProviderFailureCapture>,
}

impl ProviderFailureCaptureGuard {
    pub(super) fn install(runtime: ProviderRequestRuntime, failures: Arc<Mutex<Vec<String>>>) -> Self {
        let previous = PROVIDER_FAILURE_CAPTURE
            .lock()
            .expect("provider failure observer")
            .replace(ProviderFailureCapture { runtime, failures });
        Self { previous }
    }
}

impl Drop for ProviderFailureCaptureGuard {
    fn drop(&mut self) {
        *PROVIDER_FAILURE_CAPTURE.lock().expect("provider failure observer") = self.previous.take();
    }
}

pub(in super::super) fn record_provider_failure(error: &LLMError, runtime: &ProviderRequestRuntime) {
    let capture = PROVIDER_FAILURE_CAPTURE.lock().expect("provider failure observer").clone();
    if let Some(capture) = capture
        && capture.runtime.shares_request_state(runtime)
    {
        capture
            .failures
            .lock()
            .expect("captured provider failures")
            .push(error.to_string());
    }
}

struct ConcurrentFailureProvider {
    diagnostic: &'static str,
    barrier: Arc<tokio::sync::Barrier>,
}

#[async_trait::async_trait]
impl LLMProvider for ConcurrentFailureProvider {
    fn name(&self) -> &str {
        PROVIDER
    }

    fn supported_models(&self) -> Vec<String> {
        vec![MODEL.to_string()]
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        let _wait_result = self.barrier.wait().await;
        Err(LLMError::Network {
            message: self.diagnostic.to_string(),
            metadata: None,
        })
    }
}

#[tokio::test]
async fn provider_failure_capture_excludes_concurrent_unrelated_generation() {
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let config = CustomProviderConfig {
        name: PROVIDER.to_string(),
        request_policy: CustomProviderRequestPolicyConfig { max_retries: 0, ..Default::default() },
        ..Default::default()
    };
    // Equal provider names and policies must not merge independent agents'
    // request state. Conversely, a cloned runtime must retain its capture.
    let observed_runtime = ProviderRuntimeRegistry::new(
        std::slice::from_ref(&config),
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider(PROVIDER);
    let unrelated_runtime = ProviderRuntimeRegistry::new(
        std::slice::from_ref(&config),
        &vtcode_config::TimeoutsConfig::default(),
        Arc::new(vtcode_core::metrics::MetricsCollector::new()),
    )
    .for_provider(PROVIDER);
    let failures = Arc::new(Mutex::new(Vec::new()));
    let _failure_guard = ProviderFailureCaptureGuard::install(observed_runtime.clone(), Arc::clone(&failures));
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let observed_provider = ConcurrentFailureProvider {
        diagnostic: "observed-generation-failure",
        barrier: Arc::clone(&barrier),
    };
    let unrelated_provider = ConcurrentFailureProvider {
        diagnostic: "unrelated-generation-failure",
        barrier,
    };
    let cancellation = SessionCancellation::default();

    let (observed, unrelated) = tokio::join!(
        generate_with_retry(&observed_provider, LLMRequest::default(), &observed_runtime, &cancellation, None),
        generate_with_retry(&unrelated_provider, LLMRequest::default(), &unrelated_runtime, &cancellation, None),
    );

    assert_failed_generation(observed, &observed_runtime);
    assert_failed_generation(unrelated, &unrelated_runtime);
    assert_capture_isolated(&failures.lock().expect("isolated generation failures"));
}

fn assert_failed_generation(result: Result<LLMResponse, ProviderCallError>, runtime: &ProviderRequestRuntime) {
    assert!(matches!(result, Err(ProviderCallError::Failed(_))));
    assert_eq!(runtime.telemetry_snapshot().active_acp_requests, 0);
}

fn assert_capture_isolated(failures: &[String]) {
    assert_eq!(failures.len(), 1, "only the observed generation may enter the capture");
    assert!(failures[0].contains("observed-generation-failure"));
    assert!(!failures[0].contains("unrelated-generation-failure"));
}
