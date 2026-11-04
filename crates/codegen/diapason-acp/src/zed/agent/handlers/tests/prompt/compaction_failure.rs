//! Covers the terminal activity a failed or cancelled ACP compaction publishes.
use super::compaction::{CaptureWriter, capture_subscriber};
use super::*;
use agent_client_protocol::schema::v1::InitializeRequest;

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

    let agent_connection = install_handlers(Agent.builder().name("diapason-compaction-failure-test"), Arc::clone(agent))
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
    let agent = Arc::new(build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await);
    let session = large_compaction_session(&agent, workspace.path()).await;

    let started = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(PendingCompactionProvider::new(Arc::clone(&started)));
    let before = crate::metrics::compaction_metrics();
    let (updates, error) = run_failing_compaction_over_duplex(&agent, &session, provider, started, true).await;
    let after = crate::metrics::compaction_metrics();

    assert_eq!(error.to_string(), "ACP compaction cancelled", "a cancelled compaction must surface its own message");
    assert_failed_terminal_activity(&updates, "cancelled");
    assert!(
        after.cancelled > before.cancelled,
        "a cancelled attempt must be counted as a cancelled failure: {before:?} -> {after:?}"
    );
    assert!(after.failed > before.failed, "cancellation is a subset of failed attempts: {before:?} -> {after:?}");
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
    let agent = build_wire_test_agent_with_options(workspace.path(), Some(&config), None).await;
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
        delay: Duration::from_millis(20),
    };
    let runtime = agent.provider_runtime.for_provider("wire-test");

    let before = crate::metrics::compaction_metrics();
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
    let after = crate::metrics::compaction_metrics();

    assert!(compacted, "the compacted history must still be adopted");
    assert!(after.started > before.started, "the attempt must record a start: {before:?} -> {after:?}");
    assert!(after.compacted > before.compacted, "the terminal outcome must be recorded: {before:?} -> {after:?}");
    // The provider stalls for a controlled interval, so elapsed recording must
    // move both duration counters by a strictly positive amount. Nextest runs
    // each test in its own process, so the deltas below cover this attempt
    // alone.
    let elapsed_ms_delta = after.elapsed_ms_total - before.elapsed_ms_total;
    assert!(elapsed_ms_delta > 0, "the attempt duration must be recorded: {before:?} -> {after:?}");
    assert!(
        after.elapsed_ms_max > before.elapsed_ms_max,
        "the longest attempt duration must be recorded: {before:?} -> {after:?}"
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
