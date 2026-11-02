//! Tests provider request admission, queue telemetry, timeouts, and cancellation.
use super::*;

fn runtime(limit: usize, queue_timeout: Duration) -> ProviderRequestRuntime {
    ProviderRequestRuntime {
        provider_name: Arc::from("test"),
        provider_class: ProviderClass::Unknown,
        limit: Some(limit),
        semaphore: Some(Arc::new(Semaphore::new(limit))),
        queue_timeout,
        retry_policy: RetryPolicy::default(),
        deadline_policy: ProviderDeadlinePolicy::from_config(&CustomProviderRequestPolicyConfig::default()),
        counters: Arc::new(ProviderRequestCounters::default()),
        metrics: Arc::new(MetricsCollector::new()),
    }
}

#[tokio::test]
async fn local_limit_blocks_until_the_active_request_finishes() {
    let runtime = runtime(1, Duration::from_secs(1));
    let cancellation = SessionCancellation::default();
    let first = runtime.acquire(&cancellation).await.expect("first permit");
    assert_eq!(runtime.telemetry_snapshot().active_acp_requests, 1);
    let second = tokio::time::timeout(Duration::from_millis(20), runtime.acquire(&cancellation)).await;
    assert!(second.is_err(), "second request must remain queued");
    assert_eq!(runtime.telemetry_snapshot().acp_local_queue_depth, 0, "cancelled wait must leave the queue");

    drop(first);
    assert_eq!(runtime.telemetry_snapshot().active_acp_requests, 0);
    let _released = runtime.acquire(&cancellation).await.expect("released permit");
}

#[tokio::test]
async fn telemetry_counts_a_waiting_request() {
    let runtime = runtime(1, Duration::from_secs(1));
    let cancellation = SessionCancellation::default();
    let _first = runtime.acquire(&cancellation).await.expect("first permit");
    let queued_runtime = runtime.clone();
    let queued_cancellation = cancellation.clone();
    let queued = tokio::spawn(async move { queued_runtime.acquire(&queued_cancellation).await });
    tokio::task::yield_now().await;

    let snapshot = runtime.telemetry_snapshot();
    assert_eq!(snapshot.acp_local_queue_depth, 1);
    assert_eq!(snapshot.active_acp_requests, 1);
    assert_eq!(snapshot.configured_provider_limit, Some(1));
    assert_eq!(snapshot.circuit_breaker_state, "not_configured");

    cancellation.cancel();
    let error = queued.await.expect("queued task").expect_err("queue should be cancelled");
    assert!(matches!(error, ProviderAdmissionError::Cancelled));
    assert_eq!(runtime.telemetry_snapshot().acp_local_queue_depth, 0);
}

#[tokio::test]
async fn queued_request_times_out() {
    let runtime = runtime(1, Duration::from_millis(10));
    let cancellation = SessionCancellation::default();
    let _first = runtime.acquire(&cancellation).await.expect("first permit");

    let error = runtime.acquire(&cancellation).await.expect_err("queue should time out");
    assert!(matches!(error, ProviderAdmissionError::QueueTimeout { limit: 1, .. }));
}

#[tokio::test]
async fn cancellation_interrupts_a_queued_request() {
    let runtime = runtime(1, Duration::from_secs(1));
    let cancellation = SessionCancellation::default();
    let _first = runtime.acquire(&cancellation).await.expect("first permit");

    let queued_cancellation = cancellation.clone();
    let queued_runtime = runtime.clone();
    let queued = tokio::spawn(async move { queued_runtime.acquire(&queued_cancellation).await });
    tokio::task::yield_now().await;
    cancellation.cancel();

    let error = queued.await.expect("queued task").expect_err("queue should be cancelled");
    assert!(matches!(error, ProviderAdmissionError::Cancelled));
}

#[test]
fn registry_maps_custom_retry_policy_to_provider() {
    let provider = CustomProviderConfig {
        name: "ArLi".to_string(),
        request_policy: CustomProviderRequestPolicyConfig {
            max_in_flight_requests: Some(3),
            max_retries: 4,
            retry_initial_backoff_ms: 250,
            retry_max_backoff_ms: 5_000,
            retry_jitter: false,
            ..CustomProviderRequestPolicyConfig::default()
        },
        ..CustomProviderConfig::default()
    };
    let runtime =
        ProviderRuntimeRegistry::new(&[provider], &TimeoutsConfig::default(), Arc::new(MetricsCollector::new()))
            .for_provider("arli");

    assert_eq!(runtime.limit, Some(3));
    assert!(runtime.semaphore.is_none(), "custom-provider capacity is enforced at the shared router boundary");
    assert_eq!(runtime.retry_policy.max_attempts, 5);
    assert_eq!(runtime.retry_policy.initial_delay, Duration::from_millis(250));
    assert_eq!(runtime.retry_policy.max_delay, Duration::from_secs(5));
    assert!(runtime.retry_policy.jitter.abs() < f64::EPSILON);
    assert_eq!(runtime.deadline_policy.connect, Some(Duration::from_secs(30)));
    assert_eq!(runtime.deadline_policy.first_token, Some(Duration::from_secs(180)));
    assert_eq!(runtime.deadline_policy.stream_idle, Some(Duration::from_secs(120)));
    assert_eq!(runtime.deadline_policy.total_generation, Some(Duration::from_secs(600)));
}

#[test]
fn provider_runtime_classifies_custom_and_fallback_metrics_without_names() {
    let provider_name = "custom-provider-marker";
    let provider = CustomProviderConfig {
        name: provider_name.to_string(),
        ..CustomProviderConfig::default()
    };
    let registry =
        ProviderRuntimeRegistry::new(&[provider], &TimeoutsConfig::default(), Arc::new(MetricsCollector::new()));
    let custom = registry.for_provider(provider_name);
    let fallback = registry.for_provider("default");

    assert_eq!(custom.provider_class(), ProviderClass::Custom);
    assert_eq!(custom.provider_class().as_str(), "custom");
    assert_eq!(fallback.provider_class(), ProviderClass::Unknown);
    assert_eq!(fallback.provider_class().as_str(), "unknown");
    assert_eq!(ProviderClass::Builtin.as_str(), "builtin");
    assert!(!custom.provider_class().as_str().contains(provider_name));
    assert!(!fallback.provider_class().as_str().contains("default"));
}

#[test]
fn provider_runtime_classifies_builtin_provider_names() {
    let registry = ProviderRuntimeRegistry::new(&[], &TimeoutsConfig::default(), Arc::new(MetricsCollector::new()));

    for provider_name in ["openai", "Anthropic", "ollama-cloud"] {
        let runtime = registry.for_provider(provider_name);
        assert_eq!(runtime.provider_class(), ProviderClass::Builtin, "{provider_name}");
    }

    let unknown = registry.for_provider("vendor-gateway");
    assert_eq!(unknown.provider_class(), ProviderClass::Unknown);
}

#[test]
fn configured_custom_provider_keeps_custom_class_when_named_like_a_builtin() {
    let provider = CustomProviderConfig {
        name: "openai".to_string(),
        ..CustomProviderConfig::default()
    };
    let registry =
        ProviderRuntimeRegistry::new(&[provider], &TimeoutsConfig::default(), Arc::new(MetricsCollector::new()));

    assert_eq!(registry.for_provider("openai").provider_class(), ProviderClass::Custom);
}
