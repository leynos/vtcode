//! Publication of a compaction activity update across the ACP send boundary.

use super::*;

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
