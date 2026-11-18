//! Terminal activity published when a compaction attempt fails or is cancelled.

use super::*;

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
