//! Pins the worktree a refused launch must not leave behind, and the
//! isolation refusals that keep a resumed or restarted run out of the parent
//! workspace.

use std::process::Command;
use std::time::Duration;

use tempfile::TempDir;

use crate::config::DiapasonConfig;

use super::super::controller_child_loop::launch_checkpoints;
use super::tests::{child_record, test_config_with};
use super::*;

/// Initializes a throwaway git repository so a worktree can be created in it.
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
    let controller = SubagentController::new(test_config_with(workspace.path().to_path_buf(), vt_cfg))
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
    // The launch checkpoint is the wait this test is about, so it gets its own
    // bound: an unbounded wait would hang the suite when the checkpoint is
    // never reached, and the diagnostic has to name the launch checkpoint
    // rather than whatever the spawn returned.
    let checkpoint = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            result = &mut spawn => panic!("the gated spawn launched before the close landed: {result:?}"),
            () = entered.notified() => {}
        }
    });
    let (child_id, checkout) = match checkpoint.await {
        Ok(()) => {
            let state = controller.state.read().await;
            let (id, record) = state.children.iter().next().expect("the inserted child");
            assert_eq!(record.status, SubagentStatus::Queued, "the launch published the queued state");
            let checkout = record.worktree_path.clone().expect("the spawn created the isolation worktree");
            assert!(checkout.is_dir(), "the checkout at {} exists before the close lands", checkout.display());
            (id.clone(), checkout)
        }
        Err(_) => panic!("the launch never reached its post-queued launch checkpoint within 10 seconds"),
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
    let config = test_config_with(workspace.path().to_path_buf(), DiapasonConfig::default());
    let controller = SubagentController::new(config).await.expect("controller");
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

/// A resumed run that already settled keeps the checkout the refusal would take.
///
/// This is the race the reopen test cannot reach. The resumed run claims the
/// checkout, executes and settles before the refused launch reaches its
/// cleanup, so the record is terminal again and looks exactly like one nothing
/// ever ran in. Removing the directory then deletes the checkout the resumed
/// run's archive and transcript describe. The claim counter the run bumps as it
/// begins is what tells the two apart, and it is read under the same state lock
/// that guards the removal.
#[tokio::test]
async fn a_refused_launch_keeps_a_checkout_a_settled_resume_already_used() {
    let workspace = TempDir::new().expect("workspace");
    let config = test_config_with(workspace.path().to_path_buf(), DiapasonConfig::default());
    let controller = SubagentController::new(config).await.expect("controller");
    let checkout = workspace.path().join("agent-worker-checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let mut record = child_record("agent-worker", SubagentStatus::Closed);
    record.closed = true;
    record.worktree_path = Some(checkout.clone());
    let id = controller
        .insert_child_with_candidate_id("agent-worker", record)
        .await
        .expect("insert child");

    // The resume reopens the record, its run begins and claims the checkout,
    // and the run then settles once more — all before the refused launch's
    // cleanup runs.
    assert!(controller.reopen_single(&id).await.expect("reopen the record"));
    controller.begin_child_run(&id).await.expect("begin the resumed run");
    {
        let state = controller.state.read().await;
        let record = state.children.get(&id).expect("the resumed record");
        assert_eq!(
            record.worktree_path.as_deref(),
            Some(checkout.as_path()),
            "the resumed run executes in the checkout the refusal would reclaim"
        );
        assert_eq!(record.worktree_claim, 1, "the resumed run claims the checkout as it begins");
        assert!(record.status == SubagentStatus::Running, "the begun run must be running before it settles");
    }
    let settled = {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut(&id).expect("the resumed record");
        // The run dequeues the prompt it was reopened with, as the loop does
        // before it executes. Without that the result would re-queue the run
        // instead of settling it.
        record.dequeue_run().expect("the reopened record has a queued run");
        record.apply_result(Err(anyhow::anyhow!("the resumed run failed")));
        record.status
    };
    assert!(settled.is_terminal(), "the resumed run must be settled before the cleanup runs");

    let taken = controller.take_unstarted_spawn_worktree(&id, Some(checkout.as_path())).await;
    assert!(taken.is_none(), "a run has already used the checkout, so the refusal must not reclaim it");

    let state = controller.state.read().await;
    let record = state.children.get(&id).expect("the settled record survives");
    assert_eq!(record.worktree_path.as_deref(), Some(checkout.as_path()), "the settled run keeps its checkout");
    assert!(checkout.is_dir(), "the checkout at {} must stay on disk", checkout.display());
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
    let config = test_config_with(workspace.path().to_path_buf(), DiapasonConfig::default());
    let controller = SubagentController::new(config).await.expect("controller");
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
    let config = test_config_with(workspace.path().to_path_buf(), DiapasonConfig::default());
    let controller = SubagentController::new(config).await.expect("controller");
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
///
/// The checkout is created before it is attached to the record, so this case
/// holds for the reason it claims: a record whose isolation directory really
/// exists still reopens. A record that merely carries a path to a directory
/// nothing created would pass only if the guard never looked at the disk,
/// which is a property of the check rather than of a resumable child.
#[tokio::test]
async fn reopen_allows_a_child_that_keeps_its_isolation_worktree() {
    let workspace = TempDir::new().expect("workspace");
    let config = test_config_with(workspace.path().to_path_buf(), DiapasonConfig::default());
    let controller = SubagentController::new(config).await.expect("controller");
    let checkout = workspace.path().join("checkout");
    std::fs::create_dir_all(&checkout).expect("checkout");
    let mut record = child_record("agent-worker", SubagentStatus::Closed);
    record.spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    record.worktree_path = Some(checkout.clone());
    assert!(checkout.is_dir(), "the checkout at {} exists before the reopen", checkout.display());
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
    assert_eq!(
        record.worktree_path.as_deref(),
        Some(checkout.as_path()),
        "the reopened run keeps the checkout that still exists"
    );
}
