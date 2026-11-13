//! Covers the rate-limit notices a provider failure publishes over ACP: a
//! terminal streamed 429, a streamed 429 the retry budget can resume from, and
//! a buffered 429 that is retried before the turn answers.
use super::*;
use agent_client_protocol::schema::v1::{InitializeRequest, NewSessionRequest};

/// Drive one prompt through a duplex ACP connection and return every session
/// update the client received.
async fn prompt_updates(factory: Arc<PromptProviderFactory>) -> Vec<serde_json::Value> {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    let _test_lock = PROMPT_PROVIDER_TEST_LOCK.lock().await;
    let _factory_guard = PromptProviderFactoryGuard::install("wire-test", factory);
    let workspace = TempDir::new().expect("wire test workspace");
    let agent = Arc::new(build_wire_test_agent(workspace.path()).await);
    let (agent_channel, client_channel) = Channel::duplex();
    let (updates_tx, mut updates_rx) = mpsc::unbounded_channel();

    let agent_connection = install_handlers(
        Agent.builder().name("diapason-rate-limit-stream-test"),
        Arc::clone(&agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(&agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                drop(updates_tx.send(notification));
                Ok(())
            },
            on_receive_notification!(),
        )
        .connect_with(client_channel, async move |cx: ConnectionTo<Agent>| {
            drop(
                cx.send_request(InitializeRequest::new(acp::ProtocolVersion::V1))
                    .block_task()
                    .await?,
            );
            let session = cx
                .send_request(NewSessionRequest::new(workspace.path().to_path_buf()))
                .block_task()
                .await?;
            let response = cx
                .send_request(PromptRequest::new(
                    session.session_id,
                    vec![acp::ContentBlock::Text(acp::TextContent::new("Begin a response"))],
                ))
                .block_task()
                .await?;
            assert_eq!(response.stop_reason, acp::StopReason::EndTurn);
            Ok(())
        });

    tokio::time::timeout(Duration::from_secs(5), client_connection)
        .await
        .expect("client connection should finish")
        .expect("the rate-limited prompt should end the turn normally");
    agent_task.abort();
    drop(agent_task.await);

    std::iter::from_fn(|| updates_rx.try_recv().ok())
        .map(|notification| serde_json::to_value(&notification.update).expect("update JSON"))
        .collect()
}

/// Select the rate-limit notice from the updates a prompt produced.
fn rate_limit_notice(updates: &[serde_json::Value]) -> &serde_json::Value {
    let notice = updates
        .iter()
        .find(|update| update["_meta"]["lody"]["notice"].is_object())
        .expect("a rate-limited prompt must publish a notice over ACP");
    assert_eq!(notice["sessionUpdate"], "session_info_update");
    assert_eq!(notice["_meta"]["lody"]["notice"]["level"], "warning");
    assert_eq!(notice["_meta"]["lody"]["notice"]["source"], "provider_rate_limit");
    assert!(
        notice["_meta"]["lody"].get("rateLimits").is_none(),
        "a rate-limit notice must not fabricate a quota window"
    );
    notice
}

/// Read the notice message, requiring it to be a string.
fn notice_message(notice: &serde_json::Value) -> &str {
    notice["_meta"]["lody"]["notice"]["message"]
        .as_str()
        .expect("rate-limit notice message")
}

/// Assert a terminal 429 publishes a notice without claiming a scheduled retry.
#[tokio::test]
async fn terminal_stream_rate_limit_publishes_a_notice_without_a_scheduled_retry() {
    let updates = prompt_updates(Arc::new(|| Box::new(PartialThenRateLimitProvider))).await;
    let notice = rate_limit_notice(&updates);
    let message = notice_message(notice);

    assert!(message.contains("HTTP 429"), "the provider status must be reported: {message}");
    assert!(message.contains("Retry-After: 12"), "the provider hint must be reported: {message}");
    assert!(
        message.contains("no further automatic retry is scheduled"),
        "a terminal failure must not claim a scheduled retry: {message}"
    );
    assert!(
        !message.contains("Diapason will retry in"),
        "a terminal failure must not advertise a retry: {message}"
    );
}

/// Assert a retryable 429 publishes a notice announcing the scheduled retry.
#[tokio::test]
async fn retryable_stream_rate_limit_publishes_a_notice_announcing_the_retry() {
    let updates = prompt_updates(Arc::new(|| Box::new(RetryableRateLimitThenAnswerProvider::new()))).await;
    let notice = rate_limit_notice(&updates);
    let message = notice_message(notice);

    assert!(message.contains("HTTP 429"), "the provider status must be reported: {message}");
    assert!(message.contains("Retry-After: 0.5"), "the provider hint must be reported: {message}");
    assert!(
        message.contains("Diapason will retry in 0.5s"),
        "a retryable failure must advertise the delay it scheduled: {message}"
    );
    assert!(
        !message.contains("no further automatic retry is scheduled"),
        "a retryable failure must not deny the scheduled retry: {message}"
    );
    let retried_answer = updates.iter().any(|update| {
        update["sessionUpdate"] == "agent_message_chunk" && update["content"]["text"] == "retried answer"
    });
    assert!(retried_answer, "the scheduled retry must deliver the follow-up answer: {updates:?}");
}

/// Assert a buffered 429 publishes the notice and the scheduled retry answers.
///
/// The buffered path retries through `generate_with_retry` rather than a
/// stream, so the notice must still reach the client and the retry must still
/// deliver the answer without a stream ever opening.
#[tokio::test]
async fn buffered_rate_limit_publishes_a_notice_and_the_retry_answers() {
    let updates = prompt_updates(Arc::new(|| Box::new(BufferedRetryableRateLimitProvider::new()))).await;
    let notice = rate_limit_notice(&updates);
    let message = notice_message(notice);

    assert!(message.contains("HTTP 429"), "the provider status must be reported: {message}");
    assert!(message.contains("Retry-After: 0.5"), "the provider hint must be reported: {message}");
    assert!(
        message.contains("Diapason will retry in 0.5s"),
        "a retryable failure must advertise the delay it scheduled: {message}"
    );
    let retried_answer = updates.iter().any(|update| {
        update["sessionUpdate"] == "agent_message_chunk" && update["content"]["text"] == "retried answer"
    });
    assert!(retried_answer, "the buffered retry must deliver the follow-up answer: {updates:?}");
}

/// Interval the injected publication clock reports, in milliseconds.
const NOTICE_STEP_MS: u64 = 275;

/// Assert a failed notice send is counted and observed by a bounded span.
///
/// With no client attached the transport rejects the send. The attempt must
/// still be counted, and the span must carry the fixed operation and stage
/// plus a bounded session correlation rather than the raw session identifier.
/// The duration comes from the injected clock, so it is asserted exactly.
#[test]
fn disconnected_client_counts_a_failed_notice_publication() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should build");
    runtime.block_on(async {
        let _history_test_guard = lock_history_test_guard().await;
        let workspace = TempDir::new().expect("wire test workspace");
        let mut agent = build_wire_test_agent_with_options(workspace.path(), None, None).await;
        // This test asserts exact deltas, so it must not share the
        // process-wide counters with another test that publishes a notice.
        let notice_counts = Arc::new(crate::metrics::IsolatedPublicationRecorder::new());
        let notice_recorder: Arc<dyn crate::metrics::PublicationRecorder> = notice_counts.clone();
        agent.set_publication_recorders(notice_recorder, crate::metrics::compaction_update_recorder());
        // The send reads the clock twice, so a stepped clock makes the reported
        // duration exactly one step instead of a wall-clock measurement.
        agent.set_publication_clock(Arc::new(SteppedPublicationClock::new(Duration::from_millis(NOTICE_STEP_MS))));
        let session_id = acp::SessionId::new(Arc::from("notice-span-session"));
        let error = LLMError::RateLimit {
            metadata: Some(LLMErrorMetadata::new(
                "baseten",
                Some(429),
                Some("rate_limit_error".to_string()),
                None,
                None,
                None,
                None,
            )),
        };
        let trace = TraceCapture::default();
        let before = notice_counts.metrics();

        {
            let _guard = tracing::subscriber::set_default(Registry::default().with(trace.clone()));
            agent.publish_rate_limit_notice(&session_id, "baseten", &error, None).await;
        }

        let after = notice_counts.metrics();
        assert_eq!(
            after.attempts,
            before.attempts + 1,
            "a send that reached the transport is an attempt: {before:?} -> {after:?}"
        );
        assert_eq!(
            after.failures,
            before.failures + 1,
            "a disconnected client must fail the send: {before:?} -> {after:?}"
        );

        let captured = trace.0.lock().expect("trace capture lock").join("\n");
        for expected in [
            "span=acp_notice_publish",
            "operation=\"provider_rate_limit_notice\"",
            "stage=\"publish_rate_limit_notice\"",
            "outcome=\"failed\"",
        ] {
            assert!(captured.contains(expected), "missing {expected} in: {captured}");
        }
        let expected_elapsed = format!("elapsed_ms={NOTICE_STEP_MS}");
        assert!(
            captured.contains(&expected_elapsed),
            "the send duration must be the injected interval, not the wall clock: {captured}"
        );
        assert!(captured.contains("session_correlation="), "the span must correlate the send: {captured}");
        assert!(!captured.contains("notice-span-session"), "the raw session id must not reach a span: {captured}");
    });
}
