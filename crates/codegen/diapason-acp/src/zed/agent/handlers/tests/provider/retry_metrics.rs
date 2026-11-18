//! Covers bounded retry telemetry recorded through the perf metric sink.
//!
//! These tests assert the exact metric names, tag keys and tag values a
//! recovery emits, on both the buffered and the streaming path, so an
//! unbounded or mislabelled metric fails here rather than in production.
use super::*;

struct RetryMetricWitness<'a> {
    expected_names: &'a [&'a str],
    forbidden_marker: &'a str,
    forbid_marker_in_tag_keys: bool,
    mode: &'a str,
    provider_class: &'a str,
    error_category: &'a str,
    retry_disposition: &'a str,
}

impl<'a> RetryMetricWitness<'a> {
    fn assert_events(&self, events: &[TelemetryEvent]) {
        let names = events.iter().map(|event| event.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names.as_slice(), self.expected_names);
        assert!(events.iter().all(|event| {
            event.tags.iter().all(|(key, value)| {
                (!self.forbid_marker_in_tag_keys || !key.contains(self.forbidden_marker))
                    && !value.contains(self.forbidden_marker)
            })
        }));
        assert_eq!(
            (
                (
                    events[0].tags.get("mode").map(String::as_str),
                    events[0].tags.get("provider_class").map(String::as_str),
                ),
                (
                    events[1].tags.get("error_category").map(String::as_str),
                    events[1].tags.get("retry_disposition").map(String::as_str),
                ),
            ),
            (
                (Some(self.mode), Some(self.provider_class)),
                (Some(self.error_category), Some(self.retry_disposition)),
            )
        );
    }
}

struct RetryingStreamProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait]
impl LLMProvider for RetryingStreamProvider {
    fn name(&self) -> &str {
        "wire-test"
    }

    fn supported_models(&self) -> Vec<String> {
        vec!["wire-model".to_string()]
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }

    async fn stream(&self, _request: LLMRequest) -> Result<LLMStream, LLMError> {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(LLMError::RateLimit {
                metadata: Some(LLMErrorMetadata::new(
                    "provider-marker://model/request/session/error-code/body",
                    Some(429),
                    Some("provider-marker-code".to_string()),
                    None,
                    None,
                    Some("0".to_string()),
                    Some("provider-marker-body".to_string()),
                )),
            });
        }

        let response = LLMResponse::new("wire-model", "recovered stream");
        Ok(Box::pin(futures::stream::iter([
            Ok(LLMStreamEvent::Token { delta: "recovered stream".to_string() }),
            Ok(LLMStreamEvent::Completed { response: Box::new(response) }),
        ])))
    }

    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        panic!("retrying stream fixture must use streaming")
    }
}

#[tokio::test]
async fn buffered_retry_records_bounded_perf_metrics() {
    let provider = FlakyProvider {
        attempts: AtomicUsize::new(0),
        network_failures: 1,
        invalid_request: false,
    };
    let recorder = Arc::new(TestPerfRecorder::new());
    let metric_sink = ProviderMetricSink::test(Arc::clone(&recorder));

    let runtime = retry_runtime();
    let cancellation = SessionCancellation::default();
    let request = RetryRequest::new(&provider, LLMRequest::default(), &runtime, &cancellation);
    let response = generate_with_retry_with_observer_and_metric_sink(request, &SystemTime::now, &metric_sink)
        .await
        .expect("transient request should recover");

    assert_eq!(response.content.as_deref(), Some("recovered"));
    let events = recorder.drain().await;
    RetryMetricWitness {
        expected_names: &[
            "diapason.acp.provider_attempt_total",
            "diapason.acp.provider_retry_total",
            "diapason.acp.provider_retry_delay_ms",
            "diapason.acp.provider_attempt_total",
            "diapason.acp.provider_generation_ms",
        ],
        forbidden_marker: "retry-test",
        forbid_marker_in_tag_keys: false,
        mode: "buffered",
        provider_class: "custom",
        error_category: "Network error",
        retry_disposition: "retry_scheduled",
    }
    .assert_events(&events);
    assert!(recorder.drain().await.is_empty());
}

#[tokio::test]
async fn streaming_retry_records_bounded_perf_metrics() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("wire test session directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider_calls = Arc::clone(&calls);
    let _factory_guard = PromptProviderFactoryGuard::install(
        "wire-test",
        Arc::new(move || Box::new(RetryingStreamProvider { calls: Arc::clone(&provider_calls) })),
    );
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let session = agent
        .new_session(NewSessionRequest::new(workspace.path()))
        .await
        .expect("create wire test session");
    let recorder = Arc::new(TestPerfRecorder::new());

    let response = run_prompt_with_metric_sink(
        Arc::clone(&agent),
        PromptRequest::new(
            session.session_id,
            vec![acp::ContentBlock::Text(acp::TextContent::new(
                "exercise streaming retry metrics",
            ))],
        ),
        ProviderMetricSink::test(Arc::clone(&recorder)),
    )
    .await
    .expect("stream retry should recover");

    assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let events = recorder.drain().await;
    RetryMetricWitness {
        expected_names: &[
            "diapason.acp.provider_attempt_total",
            "diapason.acp.provider_rate_limited_total",
            "diapason.acp.provider_retry_total",
            "diapason.acp.provider_retry_delay_ms",
            "diapason.acp.rate_limit_notification_failure_total",
            "diapason.acp.provider_attempt_total",
            "diapason.acp.provider_generation_ms",
        ],
        forbidden_marker: "provider-marker",
        forbid_marker_in_tag_keys: true,
        mode: "stream",
        provider_class: "unknown",
        error_category: "Rate limit exceeded",
        retry_disposition: "retry_scheduled",
    }
    .assert_events(&events);
    assert!(recorder.drain().await.is_empty());
}
