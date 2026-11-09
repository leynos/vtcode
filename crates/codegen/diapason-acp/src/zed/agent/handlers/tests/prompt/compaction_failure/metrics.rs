//! Metrics recorded for a compaction attempt, including the injected clock.

use super::*;

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
