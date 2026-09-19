//! Covers provider attempt telemetry, deadline phases, retry policy, and failure classification.
use super::*;
use crate::zed::types::SessionCancellation;

#[test]
fn stream_poll_runs_inside_safe_attempt_span() {
    let capture = TraceCapture::default();
    let subscriber = Registry::default().with(capture.clone());
    tracing::subscriber::with_default(subscriber, || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime should build")
            .block_on(async {
                let span = tracing::info_span!(
                    "acp_provider_stream_attempt",
                    operation = "provider_stream_attempt",
                    provider = "test-provider",
                    outcome = tracing::field::Empty,
                    elapsed_ms = tracing::field::Empty,
                );
                let mut stream = futures::stream::poll_fn(|_context| {
                    tracing::info!(marker = "stream_poll", "polled provider stream");
                    std::task::Poll::Ready(Some(()))
                });
                let started_at = Instant::now();
                assert_eq!(poll_provider_stream(&mut stream, span.clone()).await, Some(()));
                record_stream_attempt(&span, started_at, "completion");
            });
    });

    let captured = capture.0.lock().expect("trace capture lock").join("\n");
    assert!(captured.contains("span=acp_provider_stream_attempt"));
    assert!(captured.contains("span=Some(\"acp_provider_stream_attempt\")"));
    assert!(captured.contains("marker=\"stream_poll\""));
    assert!(captured.contains("outcome=\"completion\""));
    assert!(captured.contains("elapsed_ms="));
    assert!(!captured.contains("prompt"));
    assert!(!captured.contains("api_key"));
    assert!(!captured.contains("/home/"));
}

#[test]
fn provider_error_telemetry_exposes_stable_network_metadata() {
    let error = LLMError::Network {
        message: "request failed".to_string(),
        metadata: Some(LLMErrorMetadata::new(
            "Arli AI",
            Some(504),
            Some("reqwest_timeout_error".to_string()),
            None,
            None,
            None,
            Some("operation timed out".to_string()),
        )),
    };

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Network error",
            status: Some(504),
            timeout_phase: None,
        }
    );
}

#[test]
fn provider_error_telemetry_exposes_acp_timeout_phase_separately() {
    let error = provider_timeout_error("wire-test", StreamTimeoutPhase::InterTokenIdle, Some(Duration::from_secs(30)));

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Network error",
            status: None,
            timeout_phase: Some("inter_token_idle"),
        }
    );
    assert!(error.to_string().contains("inter-token idle timeout"));
}

proptest! {
    #[test]
    fn streaming_eligibility_depends_only_on_provider_support_and_stop_hooks(
        supports_streaming in any::<bool>(),
        tools_allowed in any::<bool>(),
        has_stop_hooks in any::<bool>(),
    ) {
        prop_assert_eq!(
            should_allow_streaming(supports_streaming, tools_allowed, has_stop_hooks),
            supports_streaming && !has_stop_hooks,
        );
    }

    #[test]
    fn observing_stream_output_replaces_only_the_first_token_deadline(
        first_token_secs in prop::option::of(1u64..3_601),
        idle_secs in prop::option::of(1u64..3_601),
        total_secs in prop::option::of(1u64..7_201),
        observed_after_secs in 0u64..3_601,
    ) {
        let policy = ProviderDeadlinePolicy {
            connect: None,
            first_token: first_token_secs.map(Duration::from_secs),
            stream_idle: idle_secs.map(Duration::from_secs),
            total_generation: total_secs.map(Duration::from_secs),
        };
        let started_at = Instant::now();
        let observed_at = started_at + Duration::from_secs(observed_after_secs);
        let mut tracker = StreamDeadlineTracker::new(policy, started_at);
        let original_total = tracker.total;

        tracker.observe_output_at(observed_at);

        prop_assert_eq!(tracker.first_token, None);
        prop_assert_eq!(tracker.idle, deadline_from(observed_at, policy.stream_idle));
        prop_assert_eq!(tracker.total, original_total);
    }
}

#[tokio::test]
async fn buffered_generation_obeys_total_generation_timeout() {
    let result = generate_with_retry(
        &PendingProvider,
        LLMRequest::default(),
        &timeout_runtime(),
        &SessionCancellation::default(),
        None,
    )
    .await;

    let Err(ProviderCallError::Failed(error)) = result else {
        panic!("pending generation should fail at its total deadline");
    };
    assert_eq!(error.category(), "Network error");
    assert_eq!(error.status(), None);
}

#[test]
fn stream_deadline_moves_from_first_token_to_idle_without_resetting_total() {
    let policy = ProviderDeadlinePolicy {
        connect: Some(Duration::from_secs(30)),
        first_token: Some(Duration::from_secs(180)),
        stream_idle: Some(Duration::from_secs(120)),
        total_generation: Some(Duration::from_secs(600)),
    };
    let started_at = Instant::now();
    let total = deadline_from(started_at, policy.total_generation);
    let mut tracker = StreamDeadlineTracker::new(policy, started_at);

    assert_eq!(tracker.next().map(|(phase, _deadline)| phase), Some(StreamTimeoutPhase::FirstToken));
    let first_output_at = started_at + Duration::from_secs(20);
    tracker.observe_output_at(first_output_at);
    assert_eq!(tracker.next().map(|(phase, _deadline)| phase), Some(StreamTimeoutPhase::InterTokenIdle));
    assert_eq!(tracker.idle, deadline_from(first_output_at, policy.stream_idle));
    assert_eq!(tracker.total, total, "observing output must not extend the total generation deadline");
}

#[tokio::test]
async fn transient_network_failure_retries_and_recovers() {
    let provider = FlakyProvider {
        attempts: AtomicUsize::new(0),
        network_failures: 1,
        invalid_request: false,
    };

    let response =
        generate_with_retry(&provider, LLMRequest::default(), &retry_runtime(), &SessionCancellation::default(), None)
            .await
            .expect("transient request should recover");

    assert_eq!(response.content.as_deref(), Some("recovered"));
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn bad_gateway_retries_until_the_configured_budget_is_exhausted() {
    let provider = BadGatewayProvider { attempts: AtomicUsize::new(0) };

    let result =
        generate_with_retry(&provider, LLMRequest::default(), &retry_runtime(), &SessionCancellation::default(), None)
            .await;

    let Err(ProviderCallError::Failed(error)) = result else {
        panic!("persistent 502 response should exhaust the retry budget");
    };
    assert_eq!(error.category(), "Service temporarily unavailable");
    assert_eq!(error.status(), Some(502));
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn invalid_request_is_not_retried() {
    let provider = FlakyProvider {
        attempts: AtomicUsize::new(0),
        network_failures: 0,
        invalid_request: true,
    };

    let result =
        generate_with_retry(&provider, LLMRequest::default(), &retry_runtime(), &SessionCancellation::default(), None)
            .await;

    assert!(matches!(result, Err(ProviderCallError::Failed(_))));
    assert_eq!(provider.attempts.load(Ordering::SeqCst), 1);
}

#[test]
fn provider_error_telemetry_exposes_rate_limit_diagnostics() {
    let error = LLMError::RateLimit {
        metadata: Some(LLMErrorMetadata::new(
            "baseten",
            Some(429),
            Some("rate_limit_error".to_string()),
            None,
            None,
            Some("17".to_string()),
            Some("capacity temporarily exhausted".to_string()),
        )),
    };

    assert_eq!(
        provider_error_telemetry(&error),
        ProviderErrorTelemetry {
            category: "Rate limit exceeded",
            status: Some(429),
            timeout_phase: None,
        }
    );
}
