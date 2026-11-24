//! Pins the worktree a refused launch must not leave behind, and the
//! isolation refusals that keep a resumed or restarted run out of the parent
//! workspace.

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
use super::tests::child_record;
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
fn init_repository(root: &Path) {
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

/// A resume that reopened the record keeps the checkout the refusal would take.
///
/// The launch's refusal and a resume's reopen race for the same record. A
/// resume that wins re-queues the task and hands the checkout to that run, so
/// the refused launch must not reclaim a directory its own record no longer
/// owns: doing so would delete the worktree out from under the resumed child.
#[tokio::test]
async fn a_refused_launch_keeps_a_checkout_the_resume_reopened() {
    let workspace = TempDir::new().expect("workspace");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("controller");
    let checkout = workspace.path().join("agent-worker-checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let mut record = child_record("agent-worker", SubagentStatus::Closed);
    record.closed = true;
    record.worktree_path = Some(checkout.clone());
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    // The resume is the winner of the race: it clears the closure and queues
    // the task, exactly as `reopen_single` does for a resumable record.
    assert!(controller.reopen_single(&id).await.expect("reopen the record"));

    let taken = controller.take_unstarted_spawn_worktree(&id, Some(checkout.as_path())).await;
    assert!(taken.is_none(), "a resume has claimed the checkout, so the refusal must not reclaim it");

    let state = controller.state.read().await;
    let record = state.children.get(&id).expect("the reopened record survives");
    assert_eq!(record.worktree_path.as_deref(), Some(checkout.as_path()), "the resumed run keeps its checkout");
    assert!(checkout.is_dir(), "the checkout at {} must stay on disk for the resumed run", checkout.display());
}

/// A resume whose isolation worktree disappeared is refused.
///
/// The runner resolves a child's working directory from its record and falls
/// back to the parent workspace when the record carries no path. For a record
/// whose spec requested worktree isolation, that fallback would run the resumed
/// task outside the isolation it was configured for, so the resume must fail
/// closed instead of silently moving the task into the user's workspace.
#[tokio::test]
async fn resume_refuses_a_child_whose_isolation_worktree_is_gone() {
    let workspace = TempDir::new().expect("workspace");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("controller");
    let mut record = child_record("agent-worker", SubagentStatus::Closed);
    record.spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    record.worktree_path = None;
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    let error = controller
        .resume(TaskOwner::Workspace, &id)
        .await
        .expect_err("a record whose isolation checkout is gone must not be resumed");
    assert!(
        format!("{error:#}").contains("isolation worktree is gone"),
        "the refusal must name the missing isolation checkout: {error:#}"
    );

    let state = controller.state.read().await;
    let record = state.children.get(&id).expect("the record survives the refusal");
    assert_eq!(record.status, SubagentStatus::Closed, "a refused resume must not reopen the record");
    assert!(record.queued_prompts.is_empty(), "a refused resume must not queue work");
    assert!(record.handle.is_none(), "a refused resume must not launch the task");
}

/// Sending input that would restart the same record is refused identically.
///
/// `send_input` restarts a settled child without passing through the reopen
/// path, so the isolation check has to cover this entry point too.
#[tokio::test]
async fn send_input_refuses_a_child_whose_isolation_worktree_is_gone() {
    let workspace = TempDir::new().expect("workspace");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("controller");
    let mut record = child_record("agent-worker", SubagentStatus::Completed);
    record.spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    record.worktree_path = None;
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    let error = controller
        .send_input(
            TaskOwner::Workspace,
            SendInputRequest {
                target: id.clone(),
                message: Some("do more work".to_string()),
                items: Vec::new(),
                interrupt: false,
            },
        )
        .await
        .expect_err("a record whose isolation checkout is gone must not restart");
    assert!(
        format!("{error:#}").contains("isolation worktree is gone"),
        "the refusal must name the missing isolation checkout: {error:#}"
    );

    let state = controller.state.read().await;
    let record = state.children.get(&id).expect("the record survives the refusal");
    assert_eq!(record.status, SubagentStatus::Completed, "a refused restart must not queue a run");
    assert!(record.queued_prompts.is_empty(), "a refused restart must not queue the input");
}

/// The guard refuses a missing checkout, not every isolated record.
#[tokio::test]
async fn reopen_allows_a_child_that_keeps_its_isolation_worktree() {
    let workspace = TempDir::new().expect("workspace");
    let controller = SubagentController::new(test_config(workspace.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("controller");
    let mut record = child_record("agent-worker", SubagentStatus::Closed);
    record.spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    record.worktree_path = Some(workspace.path().join("checkout"));
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    assert!(
        controller.reopen_single(&id).await.expect("a record with its checkout reopens"),
        "a record whose isolation checkout is still attached must be reopenable"
    );
    let state = controller.state.read().await;
    let record = state.children.get(&id).expect("the reopened record");
    assert_eq!(record.status, SubagentStatus::Queued, "reopening queues the resumed run");
}
