//! Covers the terminal activity a failed or cancelled ACP compaction publishes.
use super::compaction::{CaptureWriter, capture_subscriber};
use super::*;
use crate::zed::agent::compaction::CompactionActivitySource;
use agent_client_protocol::schema::v1::InitializeRequest;

/// Provider stall the duration assertions are bounded by, in milliseconds.
const PROVIDER_DELAY_MS: u64 = 20;

/// Assert the started/terminal activity pair a failed compaction publishes.
fn assert_failed_terminal_activity(updates: &[serde_json::Value], expected_category: &str) {
    assert_eq!(updates.len(), 2, "a failed compaction must publish exactly one started and one terminal activity");
    let started = &updates[0];
    let finished = &updates[1];
    assert_eq!(started["sessionUpdate"], "tool_call");
    assert_eq!(started["status"], "in_progress");
    assert_eq!(started["_meta"]["lody"]["activity"]["kind"], "context_compaction");
    assert_eq!(finished["sessionUpdate"], "tool_call_update");
    assert_eq!(finished["status"], "failed");
    assert_eq!(
        started["toolCallId"], finished["toolCallId"],
        "the failed terminal update must keep the started tool call ID"
    );
    assert_eq!(finished["_meta"]["lody"]["activity"]["kind"], "context_compaction");
    assert_eq!(finished["_meta"]["lody"]["activity"]["compacted"], false);
    assert!(
        finished["_meta"]["lody"]["activity"].get("usedTokensAfter").is_none(),
        "a failed compaction must not report a post-compaction token count"
    );
    assert_eq!(
        finished["_meta"]["lody"]["activity"]["failureReason"], expected_category,
        "the terminal update must carry the closed-set failure category"
    );
}

/// Run one compaction attempt against a real duplex ACP transport.
///
/// The transport is the only way to observe the activity notifications a
/// client receives, so this drives the same `maybe_compact_session` entry
/// point the prompt handler uses and returns every published activity update
/// alongside the failure that ended the attempt.
async fn run_failing_compaction_over_duplex(
    agent: &Arc<ZedAgent>,
    session: &SessionHandle,
    provider: Arc<dyn LLMProvider>,
    started: Arc<tokio::sync::Notify>,
    cancel_after_start: bool,
) -> (Vec<serde_json::Value>, anyhow::Error) {
    let (agent_channel, client_channel) = Channel::duplex();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let captured_notifications = Arc::clone(&captured);
    let agent_attached = Arc::new(tokio::sync::Notify::new());
    let client_initialized = Arc::new(tokio::sync::Notify::new());
    let agent_attached_signal = Arc::clone(&agent_attached);
    let client_initialized_signal = Arc::clone(&client_initialized);

    let agent_connection = install_handlers(
        Agent.builder().name("diapason-compaction-failure-test"),
        Arc::clone(agent),
    )
    .connect_with(agent_channel, {
        let agent = Arc::clone(agent);
        async move |cx: ConnectionTo<Client>| {
            agent.attach_client(crate::zed::connection::ConnectionHandle::new(cx));
            agent_attached_signal.notify_one();
            std::future::pending::<agent_client_protocol::Result<()>>().await
        }
    });
    let agent_task = tokio::spawn(agent_connection);

    let client_connection = Client
        .builder()
        .on_receive_notification(
            async move |notification: acp::SessionNotification, _cx| {
                captured_notifications
                    .lock()
                    .expect("capture compaction notifications")
                    .push(notification);
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
            client_initialized_signal.notify_one();
            std::future::pending::<agent_client_protocol::Result<()>>().await
        });
    let client_task = tokio::spawn(client_connection);

    tokio::time::timeout(Duration::from_secs(5), async {
        agent_attached.notified().await;
        client_initialized.notified().await;
    })
    .await
    .expect("the ACP test transport should attach the client and finish initialisation");

    let runtime = agent.provider_runtime.for_provider("wire-test");
    let task = {
        let agent = Arc::clone(agent);
        let session = session.clone();
        tokio::spawn(async move {
            agent
                .maybe_compact_session(CompactionRequest {
                    session: &session,
                    provider: provider.as_ref(),
                    runtime: &runtime,
                    model: "wire-model",
                    tools: None,
                })
                .await
        })
    };

    tokio::time::timeout(Duration::from_secs(5), started.notified())
        .await
        .expect("provider compaction request should start");
    if cancel_after_start {
        session.cancellation.cancel();
    }
    let error = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("compaction should finish")
        .expect("compaction task should not panic")
        .expect_err("the fixture provider must fail compaction");

    let updates = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let updates = captured
                .lock()
                .expect("capture compaction notifications")
                .iter()
                .filter_map(|notification| serde_json::to_value(&notification.update).ok())
                .filter(|update| update["_meta"]["lody"]["activity"].is_object())
                .collect::<Vec<_>>();
            if updates.len() >= 2 {
                break updates;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a failed compaction must publish a terminal activity update");

    agent_task.abort();
    client_task.abort();
    drop(agent_task.await);
    drop(client_task.await);

    (updates, error)
}

#[tokio::test]
async fn cancelled_acp_compaction_publishes_a_cancelled_terminal_update() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    // An isolated recorder makes the assertions below independent of any
    // other attempt that may compact in this process.
    let counts = Arc::new(crate::metrics::IsolatedCompactionRecorder::new());
    agent.set_compaction_recorder(counts.clone());
    let session = large_compaction_session(&agent, workspace.path()).await;
    let agent = Arc::new(agent);

    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(PendingCompactionProvider::new(Arc::clone(&started)));
    let (updates, error) = run_failing_compaction_over_duplex(&agent, &session, provider, started, true).await;
    let metrics = counts.metrics();

    assert_eq!(error.to_string(), "ACP compaction cancelled", "a cancelled compaction must surface its own message");
    assert_failed_terminal_activity(&updates, "cancelled");
    assert_eq!(metrics.started, 1, "exactly one attempt must be counted as started: {metrics:?}");
    assert_eq!(metrics.cancelled, 1, "a cancelled attempt must be counted as a cancelled failure: {metrics:?}");
    assert_eq!(metrics.failed, 1, "cancellation is a subset of failed attempts: {metrics:?}");
}

#[tokio::test]
async fn failed_acp_compaction_publishes_a_provider_terminal_update() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await);
    let session = large_compaction_session(&agent, workspace.path()).await;

    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(FailingCompactionProvider::new(Arc::clone(&started)));
    let captured = CaptureWriter::default();
    let _guard = tracing::subscriber::set_default(capture_subscriber(&captured));
    let (updates, error) = run_failing_compaction_over_duplex(&agent, &session, provider, started, false).await;

    assert_eq!(
        error.to_string(),
        "ACP automatic context compaction failed",
        "provider failures must not leak their diagnostic to callers"
    );
    assert_failed_terminal_activity(&updates, "provider");

    let output = String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
    assert!(output.contains("operation=\"context_compaction\""), "the fixed operation must be logged: {output}");
    assert!(output.contains("error_category=\"provider\""), "the closed-set category must be logged: {output}");
    assert!(output.contains("elapsed_ms="), "the completion log must keep the elapsed time: {output}");
    assert!(output.contains("error_bytes="), "the suppressed detail reports its size, not its text: {output}");

    let warning = output
        .lines()
        .find(|line| line.contains(" WARN ") && line.contains("ACP automatic context compaction failed"))
        .expect("the compaction failure must reach the operator warning");
    assert!(
        !warning.contains("compaction summary request failed"),
        "provider payload text must stay out of the operator warning: {warning}"
    );
    assert!(
        warning.contains("error_bytes=") && warning.contains("detail_correlation="),
        "the operator warning must keep the bounded source detail: {warning}"
    );

    let suppressed = output
        .lines()
        .find(|line| line.contains("Suppressed ACP failure"))
        .expect("the suppressed failure must be recorded at debug");
    assert!(
        suppressed.contains("detail_correlation="),
        "the debug record must keep the bounded correlation digest: {suppressed}"
    );
    assert!(
        !suppressed.contains("compaction summary request failed"),
        "provider payload text must stay out of every log level: {suppressed}"
    );
    // The debug record and the operator warning describe the same failure, so
    // an operator correlating them must see one digest rather than two.
    let correlation = |line: &str| {
        line.split("detail_correlation=")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .expect("a correlation field value")
            .to_string()
    };
    assert_eq!(
        correlation(warning),
        correlation(suppressed),
        "the debug record and the warning must carry the same correlation digest"
    );
}

/// Assert a compaction that adopts history but stays over budget reports both.
///
/// The adopted history is committed before the admission check, so the
/// terminal update must say `compacted: true` and carry the post-compaction
/// estimate while still failing with the closed-set `admission_budget`
/// category. Reporting only the failure would hide that the session changed.
#[tokio::test]
async fn over_budget_acp_compaction_reports_the_adopted_history_and_failure() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await);
    let session = large_compaction_session(&agent, workspace.path()).await;
    let before_len = agent.resolved_messages(&session).expect("pre-compaction history").len();

    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(OverBudgetCompactionProvider::new(Arc::clone(&started)));
    let (updates, error) = run_failing_compaction_over_duplex(&agent, &session, provider, started, false).await;

    assert!(
        error.to_string().contains("still exceeds its safe provider admission budget"),
        "the over-budget failure must name the admission budget: {error}"
    );
    assert_eq!(updates.len(), 2, "an over-budget compaction publishes one started and one terminal activity");
    let started_update = &updates[0];
    let finished = &updates[1];
    assert_eq!(finished["sessionUpdate"], "tool_call_update");
    assert_eq!(finished["status"], "failed");
    assert_eq!(started_update["toolCallId"], finished["toolCallId"]);
    let activity = &finished["_meta"]["lody"]["activity"];
    assert_eq!(activity["kind"], "context_compaction");
    assert_eq!(activity["compacted"], true, "the adopted history must be reported as compacted");
    assert_eq!(activity["failureReason"], "admission_budget");

    let adopted = agent.resolved_messages(&session).expect("adopted history");
    assert!(adopted.len() < before_len, "compaction must replace the history: {before_len} -> {}", adopted.len());
    let adopted_tokens: usize = adopted.iter().map(|message| message.estimate_tokens()).sum();
    assert_eq!(
        activity["usedTokensAfter"].as_u64(),
        Some(u64::try_from(adopted_tokens).expect("token estimate fits u64")),
        "the terminal estimate must describe the history the session now holds"
    );
}

/// Assert a failed activity-update send is counted and observed by a span.
///
/// The send boundary is the only place that can tell a delivered update from
/// a rejected one, so it owns both the counters and the span. A client that
/// disconnects must therefore show up as a failure, and the span must carry
/// the fixed operation and stage rather than any session or transport detail.
#[tokio::test]
async fn failed_compaction_update_publication_is_observed_by_a_bounded_span() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    // This test asserts exact deltas, so it must not share the process-wide
    // counters with another test that publishes a compaction update.
    let update_counts = Arc::new(crate::metrics::IsolatedPublicationRecorder::new());
    let update_recorder: Arc<dyn crate::metrics::PublicationRecorder> = update_counts.clone();
    agent.set_publication_recorders(crate::metrics::notice_recorder(), update_recorder);
    let session = large_compaction_session(&agent, workspace.path()).await;
    let raw_session_id = session.data.lock().expect("ACP session data").session_id.to_string();
    let provider = CompactThenAnswerProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        context_size: 0,
        summary: "span compaction summary".to_string(),
        delay: Duration::ZERO,
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");
    let trace = TraceCapture::default();
    let before = update_counts.metrics();

    {
        let _guard = tracing::subscriber::set_default(Registry::default().with(trace.clone()));
        let compacted = agent
            .maybe_compact_session(CompactionRequest {
                session: &session,
                provider: &provider,
                runtime: &runtime,
                model: "wire-model",
                tools: None,
            })
            .await
            .expect("an update delivery failure must not replace the compaction result");
        assert!(compacted, "the compacted history must still be adopted");
    }

    let after = update_counts.metrics();
    assert_eq!(
        after.attempts,
        before.attempts + 2,
        "both activity updates are send attempts: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.failures,
        before.failures + 2,
        "a disconnected client must fail both sends: {before:?} -> {after:?}"
    );

    let captured = trace.0.lock().expect("trace capture lock").join("\n");
    for expected in [
        "span=acp_compaction_update_publish",
        "operation=\"context_compaction_update\"",
        "stage=\"publish_started\"",
        "stage=\"publish_finished\"",
        "outcome=\"failed\"",
        "elapsed_ms=",
    ] {
        assert!(captured.contains(expected), "missing {expected} in: {captured}");
    }
    assert!(captured.contains("session_correlation="), "each send must be correlated: {captured}");
    // Unrelated core logs may name the archive, so the privacy claim is scoped
    // to the ACP publication spans this test drives.
    let publish_lines = captured
        .lines()
        .filter(|line| line.contains("acp_compaction_update_publish"))
        .collect::<Vec<_>>();
    assert!(!publish_lines.is_empty(), "the publication spans must be captured: {captured}");
    for line in publish_lines {
        assert!(!line.contains(&raw_session_id), "the raw session id must not reach a span: {line}");
    }
}

/// A disconnected client must not replace the compaction result.
///
/// With no client attached every activity update fails to publish. The
/// attempt must still adopt the compacted history, record a positive duration
/// for its started and terminal states, and log the delivery failure through
/// fixed fields instead of the transport detail or the raw session identifier.
#[tokio::test]
async fn disconnected_client_keeps_the_compaction_result_and_logs_fixed_fields() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    // An isolated recorder makes the assertions below independent of any
    // other attempt that may compact in this process.
    let counts = Arc::new(crate::metrics::IsolatedCompactionRecorder::new());
    agent.set_compaction_recorder(counts.clone());
    let session = large_compaction_session(&agent, workspace.path()).await;
    let raw_session_id = session.data.lock().expect("ACP session data").session_id.to_string();
    let workspace_path = workspace.path().to_string_lossy().into_owned();
    // A controlled, positive provider interval makes the duration deltas
    // meaningful: without it the attempt can finish in under a millisecond and
    // the assertions would pass even if duration recording were removed.
    let provider = CompactThenAnswerProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        context_size: 0,
        summary: "disconnected compaction summary".to_string(),
        delay: Duration::from_millis(PROVIDER_DELAY_MS),
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");

    let captured = CaptureWriter::default();
    let _guard = tracing::subscriber::set_default(capture_subscriber(&captured));
    let compacted = agent
        .maybe_compact_session(CompactionRequest {
            session: &session,
            provider: &provider,
            runtime: &runtime,
            model: "wire-model",
            tools: None,
        })
        .await
        .expect("an update delivery failure must not replace the compaction result");
    let metrics = counts.metrics();

    assert!(compacted, "the compacted history must still be adopted");
    assert_eq!(metrics.started, 1, "exactly one attempt must be counted as started: {metrics:?}");
    assert_eq!(metrics.compacted, 1, "exactly one terminal outcome must be counted: {metrics:?}");
    // The provider stalls for a controlled interval, so elapsed recording must
    // reach at least that interval in both the running total and the recorded
    // maximum. The recorder is isolated, so the assertion does not depend on
    // any other attempt in this process.
    assert!(
        metrics.elapsed_ms_total >= PROVIDER_DELAY_MS,
        "the attempt duration must cover the provider delay: {metrics:?}"
    );
    assert!(
        metrics.elapsed_ms_max >= PROVIDER_DELAY_MS,
        "the longest attempt duration must cover the provider delay: {metrics:?}"
    );

    let output = String::from_utf8(captured.0.lock().expect("capture lock").clone()).expect("captured output is UTF-8");
    for expected in [
        "operation=\"context_compaction_update\"",
        "error_category=\"compaction_update_failed\"",
        "stage=\"publish_started\"",
        "stage=\"publish_finished\"",
    ] {
        assert!(output.contains(expected), "missing {expected} in: {output}");
    }
    // Unrelated core logs may name the archive, so the privacy claim is
    // scoped to the ACP activity-update warnings this test drives.
    let activity_lines = output
        .lines()
        .filter(|line| line.contains("context_compaction_update"))
        .collect::<Vec<_>>();
    assert_eq!(activity_lines.len(), 2, "both publish stages must log: {output}");
    for line in activity_lines {
        assert!(line.contains("session_correlation="), "the bounded correlation key must reach the log: {line}");
        assert!(line.contains("error_bytes="), "the failure size must reach the log: {line}");
        assert!(line.contains("detail_correlation="), "the bounded digest must reach the log: {line}");
        assert!(!line.contains("detail="), "the raw error text must never reach the log: {line}");
        assert!(!line.contains(&raw_session_id), "the raw session id must not reach the log: {line}");
        assert!(!line.contains(&workspace_path), "the workspace path must not reach the log: {line}");
    }
}

/// A compaction activity source with a fixed identity and a stepped clock.
///
/// Each `now()` call advances by the same increment, so the published duration
/// is that increment rather than however long the attempt actually took. This
/// is what lets the command test assert an exact `durationMs` and tool call ID.
struct FixedCompactionActivitySource {
    activity_id: uuid::Uuid,
    epoch: std::time::Instant,
    step: Duration,
    calls: std::sync::atomic::AtomicU64,
}

impl FixedCompactionActivitySource {
    /// Create a source that reports `activity_id` and steps `step` per read.
    fn new(activity_id: uuid::Uuid, step: Duration) -> Self {
        Self {
            activity_id,
            epoch: std::time::Instant::now(),
            step,
            calls: std::sync::atomic::AtomicU64::new(0),
        }
    }
}

impl CompactionActivitySource for FixedCompactionActivitySource {
    fn activity_id(&self) -> uuid::Uuid {
        self.activity_id
    }

    fn now(&self) -> std::time::Instant {
        let index = self.calls.fetch_add(1, Ordering::SeqCst);
        self.epoch + self.step * u32::try_from(index).unwrap_or(u32::MAX)
    }
}

/// Assert the command boundary takes its activity identity and clock from the
/// injected source rather than from the ambient clock and random generator.
///
/// The published tool call ID and the terminal duration are the observable
/// contract here: both must be the injected values, so the projection stays a
/// function of explicit inputs.
#[tokio::test]
async fn compaction_activity_uses_the_injected_identity_and_clock() {
    const STEP_MS: u64 = 1_500;
    const ACTIVITY_UUID: u128 = 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef;

    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    let mut agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    let activity_id = uuid::Uuid::from_u128(ACTIVITY_UUID);
    agent.set_compaction_activity_source(Arc::new(FixedCompactionActivitySource::new(
        activity_id,
        Duration::from_millis(STEP_MS),
    )));
    // An isolated recorder makes the duration assertions exact: no other
    // attempt in this process can contribute to the totals.
    let recorder = Arc::new(crate::metrics::IsolatedCompactionRecorder::new());
    let compaction_recorder: Arc<dyn crate::metrics::CompactionRecorder> = recorder.clone();
    agent.set_compaction_recorder(compaction_recorder);
    let session = large_compaction_session(&agent, workspace.path()).await;
    let raw_session_id = session.data.lock().expect("ACP session data").session_id.to_string();
    let agent = Arc::new(agent);

    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(FailingCompactionProvider::new(Arc::clone(&started)));
    let (updates, _error) = run_failing_compaction_over_duplex(&agent, &session, provider, started, false).await;

    assert_eq!(updates.len(), 2, "a failed compaction publishes one started and one terminal activity");
    assert_eq!(
        updates[0]["toolCallId"],
        format!("activity:compaction:{raw_session_id}:{activity_id}"),
        "the started activity must carry the injected identity"
    );
    assert_eq!(
        updates[1]["_meta"]["lody"]["activity"]["durationMs"], STEP_MS,
        "the terminal duration must come from the injected clock, not the wall clock"
    );
    // The duration the activity reports and the duration the metrics record are
    // the same injected interval, so doubling either recording would fail here
    // rather than merely satisfy a wall-clock lower bound.
    let metrics = recorder.metrics();
    assert_eq!(metrics.started, 1, "exactly one attempt must be counted: {metrics:?}");
    assert_eq!(metrics.failed, 1, "the attempt must be counted as failed: {metrics:?}");
    assert_eq!(
        metrics.elapsed_ms_total, STEP_MS,
        "the recorded total must be the injected interval, not the wall clock: {metrics:?}"
    );
    assert_eq!(
        metrics.elapsed_ms_max, STEP_MS,
        "the recorded maximum must be the injected interval, not the wall clock: {metrics:?}"
    );
}

/// Assert a real attempt is visible through the public, process-wide metrics.
///
/// The isolated recorders above prove exact deltas without racing. This test
/// takes the other half of the contract: production wiring records through the
/// process-wide recorder, so `compaction_metrics()` must reflect the attempt.
/// Another test may compact concurrently, so every assertion is a monotonic
/// lower bound on the shared counters rather than an exact delta.
#[tokio::test]
async fn production_recorder_publishes_an_attempt_through_the_public_metrics() {
    let _history_test_guard = lock_history_test_guard().await;
    let session_dir = TempDir::new().expect("temporary ACP session archive directory");
    let _session_dir_guard = EnvGuard::set(SESSION_DIR_ENV, session_dir.path());
    use assert_fs::TempDir;

    let workspace = TempDir::new().expect("temporary compaction workspace");
    let config = compaction_config(true);
    // No recorder is substituted, so this attempt uses the production wiring.
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
    let session = large_compaction_session(&agent, workspace.path()).await;
    let provider = CompactThenAnswerProvider {
        requests: Arc::new(Mutex::new(Vec::new())),
        context_size: 0,
        summary: "public metrics compaction summary".to_string(),
        delay: Duration::from_millis(PROVIDER_DELAY_MS),
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");
    let before = crate::metrics::compaction_metrics();

    let compacted = agent
        .maybe_compact_session(CompactionRequest {
            session: &session,
            provider: &provider,
            runtime: &runtime,
            model: "wire-model",
            tools: None,
        })
        .await
        .expect("a compactable session must compact");

    let after = crate::metrics::compaction_metrics();
    assert!(compacted, "the attempt must adopt the compacted history");
    assert!(after.started > before.started, "the attempt must be counted as started: {before:?} -> {after:?}");
    assert!(
        after.compacted > before.compacted,
        "the attempt must be counted as compacted: {before:?} -> {after:?}"
    );
    assert!(
        after.elapsed_ms_total >= before.elapsed_ms_total + PROVIDER_DELAY_MS,
        "the public duration total must cover the provider delay: {before:?} -> {after:?}"
    );
    assert!(
        after.elapsed_ms_max >= PROVIDER_DELAY_MS,
        "the public duration maximum must cover the provider delay: {after:?}"
    );
    assert!(
        after.failed >= after.cancelled,
        "the published subset invariant must hold in every snapshot: {after:?}"
    );
}

/// Interval the injected publication clock reports for the send, in
/// milliseconds.
const COMPACTION_UPDATE_STEP_MS: u64 = 640;

/// Assert the compaction activity update reports the injected clock interval.
///
/// The publication boundary is driven directly rather than through a whole
/// compaction attempt, so the assertion is about the send's own timing. With
/// no client attached the send fails deterministically while still reading the
/// clock before and after, so the recorded `elapsed_ms` must be exactly one
/// step: either a doubled read or a wall-clock measurement would report a
/// different value.
#[test]
fn compaction_update_reports_the_injected_send_duration() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime should build");
    runtime.block_on(async {
        let _history_test_guard = lock_history_test_guard().await;
        let workspace = TempDir::new().expect("wire test workspace");
        let mut agent = build_wire_test_agent_with_options(workspace.path(), None, None).await;
        // Isolated recorders and a stepped clock keep the assertions exact: no
        // other publisher in this process can perturb the counts or the span.
        let update_counts = Arc::new(crate::metrics::IsolatedPublicationRecorder::new());
        let update_recorder: Arc<dyn crate::metrics::PublicationRecorder> = update_counts.clone();
        agent.set_publication_recorders(crate::metrics::notice_recorder(), update_recorder);
        agent.set_publication_clock(Arc::new(SteppedPublicationClock::new(Duration::from_millis(
            COMPACTION_UPDATE_STEP_MS,
        ))));
        let session_id = acp::SessionId::new(Arc::from("compaction-update-clock-session"));
        let update = acp::SessionUpdate::ToolCall(acp::ToolCall::new(
            "activity:compaction:clock-session",
            "Compact conversation context",
        ));
        let trace = TraceCapture::default();

        {
            let _guard = tracing::subscriber::set_default(Registry::default().with(trace.clone()));
            agent
                .publish_compaction_update(
                    &session_id,
                    Ok(update),
                    "clock-session-correlation",
                    "publish_compaction_update",
                    "serialize_compaction_update",
                )
                .await;
        }

        let metrics = update_counts.metrics();
        assert_eq!(metrics.attempts, 1, "a send that reached the transport is one attempt: {metrics:?}");
        assert_eq!(metrics.failures, 1, "a disconnected client must fail the send: {metrics:?}");

        let captured = trace.0.lock().expect("trace capture lock").join("\n");
        for expected in [
            "span=acp_compaction_update_publish",
            "operation=\"context_compaction_update\"",
            "stage=\"publish_compaction_update\"",
            "outcome=\"failed\"",
            "session_correlation=",
        ] {
            assert!(captured.contains(expected), "missing {expected} in: {captured}");
        }
        let expected_elapsed = format!("elapsed_ms={COMPACTION_UPDATE_STEP_MS}");
        assert!(
            captured.contains(&expected_elapsed),
            "the send duration must be the injected interval, not the wall clock: {captured}"
        );
        assert!(
            !captured.contains("compaction-update-clock-session"),
            "the raw session id must not reach a span: {captured}"
        );
    });
}
