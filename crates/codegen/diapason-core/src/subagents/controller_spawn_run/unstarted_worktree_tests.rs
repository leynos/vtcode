//! Pins the worktree a refused launch must not leave behind.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::config::constants::models;
use crate::config::types::ReasoningEffortLevel;
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;

use super::super::controller_child_loop::launch_checkpoints;
use super::*;

/// Builds the controller configuration the launch tests run against.
fn test_config(workspace_root: PathBuf, vt_cfg: DiapasonConfig) -> SubagentControllerConfig {
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: models::openai::GPT_5_6_SOL.to_string(),
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

/// Initialises a throwaway git repository so a worktree can be created in it.
///
/// Every command runs with an explicit identity and with signing and hooks
/// disabled, so a developer's global or system Git configuration cannot change
/// or block the seed commit.
fn init_repository(root: &std::path::Path) {
    let hooks = TempDir::new().expect("isolated hooks directory");
    let hooks_path = hooks.path().to_path_buf();
    let run = |args: &[&str]| {
        let output = Command::new("git")
            .arg("-c")
            .arg("user.name=diapason-test")
            .arg("-c")
            .arg("user.email=test@diapason.dev")
            .arg("-c")
            .arg("commit.gpgsign=false")
            .arg("-c")
            .arg("tag.gpgsign=false")
            .arg("-c")
            .arg(format!("core.hooksPath={}", hooks_path.display()))
            .args(args)
            .current_dir(root)
            .output()
            .expect("spawn git");
        assert!(output.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&output.stderr));
    };
    run(&["init", "-q"]);
    std::fs::write(root.join("README.md"), "seed\n").expect("write seed file");
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "seed"]);
}

/// A close that lands between insertion and launch reclaims the checkout.
///
/// The record becomes visible before the launch starts it, so a close can
/// settle it in that window and the launcher then refuses to resurrect it. The
/// spawn has already created an isolation worktree, and no record will ever run
/// in it, so the failure must reclaim the checkout and still report the
/// refusal that caused it.
#[tokio::test]
async fn a_launch_refused_after_insertion_reclaims_its_worktree() {
    let workspace = TempDir::new().expect("workspace");
    init_repository(workspace.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.enabled = true;
    vt_cfg.subagents.auto_delegate_read_only = true;
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf(), vt_cfg))
        .await
        .expect("controller");
    {
        let mut state = controller.state.write().await;
        let spec = state
            .discovered
            .effective
            .iter_mut()
            .find(|spec| spec.name == "explorer")
            .expect("explorer spec");
        spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    }

    let (entered, release) = launch_checkpoints::arm(&controller.state);
    let spawn = controller.spawn(SpawnAgentRequest {
        agent_type: Some("explorer".to_string()),
        message: Some("Inspect the repository layout.".to_string()),
        ..SpawnAgentRequest::default()
    });
    tokio::pin!(spawn);
    let (child_id, checkout) = tokio::select! {
        result = &mut spawn => panic!("the gated spawn launched before the close landed: {result:?}"),
        () = entered.notified() => {
            let state = controller.state.read().await;
            let (id, record) = state.children.iter().next().expect("the inserted child");
            assert_eq!(record.status, SubagentStatus::Queued, "the launch published the queued state");
            let checkout = record.worktree_path.clone().expect("the spawn created the isolation worktree");
            assert!(
                checkout.is_dir(),
                "the checkout at {} exists before the close lands",
                checkout.display()
            );
            (id.clone(), checkout)
        }
    };

    let closed = controller
        .close(TaskOwner::Workspace, &child_id)
        .await
        .expect("close the parked launch");
    assert_eq!(closed.status, SubagentStatus::Closed);
    release.notify_one();

    let error = tokio::time::timeout(Duration::from_secs(10), spawn)
        .await
        .expect("the refused launch returns")
        .expect_err("a launch that lost its record to a close must be refused");
    assert!(
        format!("{error:#}").contains("was closed before it could start"),
        "the caller keeps the refusal that caused the reclaim: {error:#}"
    );

    // Inspect the exact checkout captured at the checkpoint, and fail on any
    // error other than its absence: a permission or I/O fault must not read as
    // a successful cleanup.
    let inspection = std::fs::metadata(&checkout);
    assert!(
        matches!(&inspection, Err(error) if error.kind() == std::io::ErrorKind::NotFound),
        "a launch refused before it started must remove the checkout at {}: {inspection:?}",
        checkout.display()
    );
    let state = controller.state.read().await;
    let record = state.children.get(&child_id).expect("the record survives the refused launch");
    assert_eq!(record.status, SubagentStatus::Closed, "the close keeps its terminal outcome");
    assert!(record.worktree_path.is_none(), "the reclaimed checkout must not stay attached to the record");
}
