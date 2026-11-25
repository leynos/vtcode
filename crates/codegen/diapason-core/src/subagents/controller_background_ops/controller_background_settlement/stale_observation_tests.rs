//! Pins the phase check that discards an absence observed before a commit.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;

use super::super::BackgroundRunRequest;
use super::super::controller_background_run::gate_next_background_commit;
use super::super::tests::support::{write_background_demo_agent, write_sleeping_demo_script};
use super::*;

fn test_config(workspace_root: PathBuf, vt_cfg: DiapasonConfig) -> SubagentControllerConfig {
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: "gpt-5.6-sol".to_string(),
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
    }
}

/// A missing session observed while `Starting` is discarded after the same
/// launch commits.
///
/// Refresh reads the record and then observes its exec session outside the
/// lock. A spawn that has not registered yet reads as missing, and the
/// launch can commit `Running` before that observation is applied. Acting
/// on the stale absence would fail the launch, or spend its allowance on a
/// duplicate replacement. The committed launch is produced by the existing
/// commit gate, so the observation's phase is the one a refresh really sees
/// while the spawn is still in flight.
#[tokio::test]
async fn a_missing_observation_from_before_the_commit_does_not_settle_the_launch() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.auto_restore = true;
    let controller = SubagentController::new(test_config(temp.path().to_path_buf(), vt_cfg))
        .await
        .expect("controller");

    let (entered, release) = gate_next_background_commit(&controller.state);
    let launch = controller.ensure_background_record_running(BackgroundRunRequest {
        agent_name: "background-demo".to_string(),
        stable_id: Some("background-worker".to_string()),
        restart_attempts: 0,
        overrides: None,
        expected_exec_session_id: None,
    });
    tokio::pin!(launch);
    let (exec_session_id, observed_status) = tokio::select! {
        result = &mut launch => {
            panic!("the gated launch settled before its observation: {result:?}")
        },
        () = entered.notified() => {
            let state = controller.state.read().await;
            let record = state.background_children.get("background-worker").expect("record");
            assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
            (record.exec_session_id.clone(), record.status)
        }
    };

    release.notify_one();
    let entry = launch.await.expect("the launch commits");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Running);

    let respawn = controller
        .update_background_record_state_observed(
            "background-worker",
            Some(exec_session_id.as_str()),
            Some(observed_status),
            None,
        )
        .await
        .expect("a stale absence is not a settlement failure");
    assert!(respawn.is_none(), "a stale absence must not schedule a replacement");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Running,
        "the launch that committed must not be settled from an absence observed before it"
    );
    assert_eq!(record.restart_attempts, 0, "a stale absence must not spend the restart allowance");
    drop(state);

    controller.signal_shutdown().await;
}
