//! Exercises Lody subagent management: liveness-projected listing and the
//! output-tail transport bound.

use super::*;
use assert_fs::TempDir;
use diapason_config::DiapasonConfig;
use diapason_core::config::types::ReasoningEffortLevel;
use diapason_core::subagents::SubagentControllerConfig;
use diapason_core::tools::exec_session::ExecSessionManager;
use diapason_core::tools::registry::PtySessionManager;

fn persisted_running_record_json() -> Value {
    serde_json::json!({
        "records": [{
            "id": "background-worker",
            "agent_name": "worker",
            "display_label": "worker",
            "description": "worker",
            "source": "builtin",
            "color": null,
            "owner_session_id": "session-a",
            "session_id": "session-worker",
            "exec_session_id": "exec-missing",
            "desired_enabled": false,
            "status": "running",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": "2026-08-28T00:00:00Z",
            "ended_at": null,
            "pid": 4321,
            "prompt": "work",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    })
}

/// `_lody/subagents/list` is a read-only query: it projects cached records
/// with liveness overlaid, so an exited process is not reported as running
/// and a read never mutates the record set or rewrites the ledger.
#[tokio::test]
async fn listing_projects_liveness_without_mutating_records() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(state_dir.join("background_subagents.json"), persisted_running_record_json().to_string())
        .expect("write state");

    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(temp.path().to_path_buf(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(temp.path().to_path_buf(), pty_sessions.clone());
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
                pty_manager: pty_sessions.manager().clone(),
                managed_background_runtime: false,
            },
            Some("session-a"),
        )
        .await
        .expect("scoped controller"),
    );
    let controllers = [Arc::clone(&controller)];

    let cached = background_status_entries(&controllers).await;
    assert_eq!(
        cached.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the cached projection reports the persisted state"
    );

    let ledger_path = state_dir.join("background_subagents.json");
    let ledger_before = std::fs::read_to_string(&ledger_path).expect("read ledger before list");

    let observed = observed_background_entries(&controllers).await;
    assert_eq!(
        observed.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Stopped],
        "listing must observe the exit instead of reporting a stale running process"
    );

    let after = background_status_entries(&controllers).await;
    assert_eq!(
        after.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the read-only query must not mutate the record set"
    );

    let ledger_after = std::fs::read_to_string(&ledger_path).expect("read ledger after list");
    assert_eq!(ledger_before, ledger_after, "the read-only query must not rewrite the persisted ledger");
}

/// The output-tail bound is an ACP transport contract, so the adapter owns
/// the default and the hard maximum rather than the domain controller.
///
/// These are the ACP transport contract literals: an omitted `tail` means
/// 200 lines, 10_000 is the inclusive maximum, and 10_001 is rejected.
#[test]
fn output_tail_defaults_and_rejects_values_above_the_transport_bound() {
    assert_eq!(normalize_output_tail_lines(None).expect("default tail size"), 200);
    assert_eq!(normalize_output_tail_lines(Some(10_000)).expect("boundary tail size"), 10_000);
    let error =
        normalize_output_tail_lines(Some(10_001)).expect_err("a tail above the transport bound must be rejected");
    assert!(error.contains("10000"), "the rejection must name the bound: {error}");
}
