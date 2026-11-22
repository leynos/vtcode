//! Pins the privacy boundary of the background liveness diagnostic.
//!
//! A task that closes while a listing observes it can make the exec-session
//! read fail for a reason other than "gone". That fault is not an observed
//! exit, so the listing keeps the cached status and records a bounded
//! `Unobserved` reason. The reason travels beside the task id into the ACP
//! debug log, and a storage backend quotes the record it was asked for in its
//! own message, so the exec-session identifier must be redacted before either
//! surface sees it.

use super::*;
use crate::zed::agent::test_log_capture::capturing_async;
use tracing_subscriber::filter::LevelFilter;

/// The exec-session id a task names while its storage read fails mid-close.
const CLOSING_EXEC_SESSION: &str = "exec-closing-worker";

/// A close race must report the failure without spreading the exec-session id.
///
/// The record keeps its cached status, the projection is `Unobserved` with a
/// redacted reason, and neither that reason nor the ACP debug log event names
/// the session the backend quoted.
#[tokio::test]
async fn close_race_liveness_diagnostic_never_publishes_the_exec_session_id() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(
        state_dir.join("background_subagents.json"),
        persisted_running_record_json(CLOSING_EXEC_SESSION).to_string(),
    )
    .expect("write state");

    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(temp.path().to_path_buf(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(temp.path().to_path_buf(), pty_sessions.clone());
    // A storage backend names the record it was asked for in its own message,
    // which is how an identifier would reach the reason and the log.
    exec_sessions.fail_snapshot_reads(format!("PTY session '{CLOSING_EXEC_SESSION}' closed while reading"));
    let controller = Arc::new(
        SubagentController::new_with_background_owner(
            SubagentControllerConfig {
                workspace_root: temp.path().to_path_buf(),
                parent_session_id: "session-a".to_string(),
                parent_model: "test-model".to_string(),
                parent_provider: "openai".to_string(),
                parent_reasoning_effort: ReasoningEffortLevel::Medium,
                api_key: "test-key".to_string(),
                vt_cfg,
                openai_chatgpt_auth: None,
                depth: 0,
                workspace_gated: false,
                exec_sessions,
                output_reader: None,
                pty_manager: pty_sessions.manager().clone(),
                managed_background_runtime: false,
                lock_scope: None,
            },
            Some("session-a"),
        )
        .await
        .expect("scoped controller"),
    );
    let controllers = [Arc::clone(&controller)];

    let (observed, events) =
        capturing_async(LevelFilter::DEBUG, observed_background_entries(&controllers, TaskOwner::Session("session-a")))
            .await;

    assert_eq!(
        observed.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "a failed liveness read must not fabricate a terminal status"
    );
    assert!(observed[0].ended_at.is_none(), "an unobserved record keeps its active terminal state");

    let projected = controller
        .background_status_entries_with_liveness(TaskOwner::Session("session-a"))
        .await;
    let reason = match &projected[0].liveness {
        BackgroundLiveness::Unobserved { reason } => reason,
        BackgroundLiveness::Observed => panic!("a failed liveness read must be reported as unobserved"),
    };
    assert!(
        !reason.contains(CLOSING_EXEC_SESSION),
        "the reason must not carry the exec-session id, got: {reason}"
    );
    assert!(
        reason.contains("<session>"),
        "the redaction must stay visible so the operator knows why, got: {reason}"
    );

    let log = events.join("\n");
    assert!(
        log.contains("Background liveness read failed"),
        "the listing must record the failed read, got: {log}"
    );
    assert!(!log.contains(CLOSING_EXEC_SESSION), "the ACP log must not carry the exec-session id, got: {log}");
}
