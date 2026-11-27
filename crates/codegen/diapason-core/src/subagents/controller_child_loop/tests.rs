//! Regressions for the child launch path and for runtime publication.
//!
//! These cases pin one ordering contract: a record that was settled before a
//! launch or a publication reached the state lock must not be treated as if it
//! were still live. The graft window and the launch cases that interleave with
//! a resumed loop live in [`graft_publication`], which this module declares.

use super::*;
use crate::core::threads::ThreadManager;
use crate::subagents::controller_tree::ChildRuntimePublication;
use crate::subagents::types::ChildRunRequest;
use crate::tools::registry::PtySessionManager;
use std::time::Duration;
use tempfile::TempDir;

mod graft_publication;

/// The built-in explorer spec a child record needs.
fn explorer_spec() -> SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer spec")
}

fn test_controller_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
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
    }
}

fn nested_controller_config(workspace_root: PathBuf, parent_session_id: &str) -> SubagentControllerConfig {
    let mut config = test_controller_config(workspace_root);
    config.parent_session_id = parent_session_id.to_string();
    config.depth = 1;
    // Mirror `child_controller_for`: a nested scope runs under the managed
    // background runtime and therefore never owns background work.
    config.managed_background_runtime = true;
    config
}

fn child_record(
    id: &str,
    parent_thread_id: &str,
    spec: &SubagentSpec,
    status: SubagentStatus,
    depth: usize,
) -> ChildRecord {
    ChildRecord {
        id: id.to_string(),
        session_id: format!("{id}-session"),
        parent_thread_id: parent_thread_id.to_string(),
        spec: spec.clone(),
        display_label: subagent_display_label(spec),
        status,
        background: false,
        depth,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        completed_at: status.is_terminal().then_some(Utc::now()),
        summary: None,
        error: None,
        archive_metadata: None,
        archive_path: None,
        transcript_path: None,
        effective_config: None,
        stored_messages: Vec::new(),
        last_prompt: Some(format!("prompt-{id}")),
        queued_prompts: VecDeque::new(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        thread_handle: None,
        handle: None,
        notify: Arc::new(Notify::new()),
        worktree_path: None,
        worktree_claim: 0,
        child_controller: None,
        closed: false,
    }
}

/// A rejected attachment must not publish the runtime of a runner that
/// never started.
///
/// The record is written under the same state lock that validates the
/// attachment, so the rejection has to happen first. Recording the archive
/// path, the effective config and the thread handle before the check
/// leaves a record that claims a live run even though the run failed to
/// attach its controller and will not continue.
#[tokio::test]
async fn a_rejected_attachment_publishes_no_child_runtime() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let controller = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("controller");
    let spec = explorer_spec();
    let existing = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root.clone(), "child-session"))
            .await
            .expect("existing controller"),
    );
    let replacement = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "replacement-session"))
            .await
            .expect("replacement controller"),
    );

    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Running, 1);
    record.child_controller = Some(Arc::clone(&existing));
    controller.state.write().await.children.insert("child".to_string(), record);

    let threads = ThreadManager::new();
    let publication = ChildRuntimePublication {
        archive_metadata: build_subagent_archive_metadata(
            Path::new("/workspace"),
            "test-model",
            "openai",
            "default",
            "medium",
            "parent-session",
            false,
        ),
        archive_path: PathBuf::from("child-archive.json"),
        effective_config: DiapasonConfig::default(),
        thread_handle: threads.start_thread_with_identifier("child", ThreadBootstrap::new(None)),
        child_controller: Some(Arc::clone(&replacement)),
    };
    let mut state = controller.state.write().await;
    let rejection = SubagentController::publish_child_runtime(&mut state, "child", publication);
    let rejection = rejection.expect_err("an occupied record must reject the attachment");
    assert!(format!("{rejection}").contains("already attached"), "{rejection}");

    let record = state.children.get("child").expect("the record survives the rejection");
    assert!(record.archive_metadata.is_none(), "a rejected attachment published metadata");
    assert!(record.archive_path.is_none(), "a rejected attachment published an archive path");
    assert!(record.effective_config.is_none(), "a rejected attachment published a config");
    assert!(record.thread_handle.is_none(), "a rejected attachment published a thread handle");
    let tracked = match record.child_controller.as_ref() {
        Some(attached) => Arc::ptr_eq(attached, &existing),
        None => false,
    };
    assert!(tracked, "the rejected attachment must keep the tracked controller");
    drop(state);

    existing.signal_shutdown().await;
    replacement.signal_shutdown().await;
}

/// An isolated child that lost its checkout before runtime capture is refused.
///
/// The record is read once when the run begins and the workspace is resolved
/// later, so the checkout can be reclaimed in between. The resolver otherwise
/// falls back to the parent workspace, which would run an
/// `isolation = "worktree"` task outside its isolation and archive it against
/// the wrong root. The run must fail closed instead.
#[tokio::test]
async fn an_isolated_child_without_its_checkout_is_refused_before_resolving_a_workspace() {
    let temp = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    let mut spec = explorer_spec();
    spec.isolation = Some(diapason_config::IsolationMode::Worktree);
    let context = ChildExecutionContext {
        spec,
        session_id: "agent-explorer-session".to_string(),
        parent_thread_id: "parent-session".to_string(),
        bootstrap_messages: Vec::new(),
        display_label: "Explorer".to_string(),
        background: false,
        // The checkout was reclaimed between `begin_child_run` and here.
        worktree_path: None,
        existing_child_controller: None,
    };

    let resolved = controller
        .resolve_child_runtime(
            &context,
            &ChildRunRequest {
                prompt: "Inspect the repository.".to_string(),
                max_turns: None,
                model_override: None,
                reasoning_override: None,
            },
        )
        .await;
    let Err(error) = resolved else {
        panic!("an isolated child without its checkout must not resolve a workspace");
    };
    assert!(
        format!("{error:#}").contains("isolation worktree is gone"),
        "the refusal must name the missing isolation checkout: {error:#}"
    );
}

/// A cancel that succeeds while a child is queued must stop it starting.
///
/// `launch_child` publishes `Queued` and then starts the run. If the launch
/// resurrects the record after the cancel — or publishes its handle without
/// re-checking the record it is about to run — the cancelled task starts
/// anyway and takes its prompt back out of the queue, so the cancel's
/// success no longer means the task stopped.
#[tokio::test]
async fn a_cancelled_queued_child_is_not_launched() {
    let temp = TempDir::new().expect("tempdir");
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    let spec = explorer_spec();
    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Queued, 1);
    record.queued_prompts.push_back("delegated work".to_string());
    controller.state.write().await.children.insert("child".to_string(), record);

    let closed = controller.close(TaskOwner::Workspace, "child").await;
    let closed = closed.expect("cancelling the queued child must succeed");
    assert_eq!(closed.status, SubagentStatus::Closed);

    let launch = controller.launch_child("child").await;
    assert!(launch.is_err(), "a cancelled child must not be launched: {launch:?}");

    let state = controller.state.read().await;
    let record = state.children.get("child").expect("the record survives the refused launch");
    assert_eq!(record.status, SubagentStatus::Closed, "the launch must not resurrect a closed record");
    assert!(record.handle.is_none(), "a refused launch must not publish a task handle");
    assert_eq!(record.queued_prompts.len(), 1, "a refused launch must not consume the prompt");
}

/// A close that lands between the queued publication and the launch's state
/// lock still stops the child starting.
///
/// The earlier cancellation test closes the record before the launch begins,
/// so the launch's very first state lock already sees it. This one parks the
/// launch in the window the earlier test cannot reach: the queued status is
/// already published, the launch has released the state lock, and no handle
/// exists yet. A close that succeeds there must be honoured — otherwise the
/// launch starts a task whose prompt the caller was told had been cancelled.
#[tokio::test]
async fn a_close_between_the_queued_publication_and_the_launch_stops_the_start() {
    let temp = TempDir::new().expect("tempdir");
    let controller = Arc::new(
        SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller"),
    );
    let spec = explorer_spec();
    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Queued, 1);
    record.queued_prompts.push_back("delegated work".to_string());
    controller.state.write().await.children.insert("child".to_string(), record);

    let (entered, release) = launch_checkpoints::arm(&controller.state);
    let launch = tokio::spawn({
        let controller = Arc::clone(&controller);
        async move { controller.launch_child("child").await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the launch must reach the checkpoint after publishing the queued status");

    // The record is queued but no handle exists yet, so this is the close the
    // launch's re-check has to observe. A close that waits on a lock the parked
    // launch holds is a failure, not a hang: bound it so the test reports the
    // broken ordering.
    let closed = tokio::time::timeout(Duration::from_secs(5), controller.close(TaskOwner::Workspace, "child"))
        .await
        .expect("the close must not wait on the parked launch");
    let closed = closed.expect("cancelling the parked launch must succeed");
    assert_eq!(closed.status, SubagentStatus::Closed);

    release.notify_one();
    let launch = launch.await.expect("the launch task must not panic");
    let rejection = launch.expect_err("a launch that lost its record to a close must be refused");
    assert!(
        format!("{rejection}").contains("was closed before it could start"),
        "the rejection must name the close that overtook the launch: {rejection}"
    );

    let state = controller.state.read().await;
    let record = state.children.get("child").expect("the record survives the refused launch");
    assert!(record.handle.is_none(), "a refused launch must not publish a task handle");
    assert_eq!(record.queued_prompts.len(), 1, "a refused launch must leave the queued prompt for a later resume");
}

/// A superseded child loop cannot withdraw the handle of the loop replacing it.
///
/// A resume can start a new loop while the previous one is still finishing, and
/// the record still carries the old handle until that loop goes idle. Two
/// things must hold: publishing the new handle must abort the displaced task
/// rather than overwrite it, and the displaced loop's own idle bookkeeping must
/// leave the new handle in place. Otherwise a later close finds no handle to
/// abort and the replacement outlives the record it belongs to.
#[tokio::test]
async fn a_superseded_child_loop_cannot_clear_the_handle_that_replaced_it() {
    let temp = TempDir::new().expect("tempdir");
    let controller = Arc::new(
        SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller"),
    );
    let spec = explorer_spec();
    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Running, 1);
    // The loop that is finishing: its task is alive and its handle is on the
    // record, exactly as it is between a run returning and the loop going idle.
    let (superseded_alive_tx, superseded_alive_rx) = tokio::sync::oneshot::channel::<()>();
    record.handle = Some(tokio::spawn(async move {
        let _alive = superseded_alive_tx;
        std::future::pending::<()>().await;
    }));
    controller.state.write().await.children.insert("child".to_string(), record);

    // The replacement loop is a real task that stays alive, so the handle the
    // record carries names a task that can still be aborted. Its own idle
    // bookkeeping is gated so the stale loop's attempt below cannot race it.
    let gate = Arc::new(Notify::new());
    let replacement = {
        let controller = Arc::clone(&controller);
        let gate = Arc::clone(&gate);
        tokio::spawn(async move {
            gate.notified().await;
            controller.mark_child_loop_idle("child").await;
            std::future::pending::<()>().await;
        })
    };
    let replacement_id = replacement.id();
    {
        let mut state = controller.state.write().await;
        let record = state.children.get_mut("child").expect("the record");
        // Publish the handle through the production replacement operation, so
        // the abort this test observes is the one `launch_child` relies on and
        // not a call the test makes on its own.
        publish_child_loop_handle(record, replacement);
    }
    let displaced = tokio::time::timeout(Duration::from_secs(5), superseded_alive_rx).await;
    assert!(
        matches!(displaced, Ok(Err(_))),
        "publishing a replacement handle must abort the displaced loop, got {displaced:?}"
    );

    // Bookkeeping that cannot show the handle is its own cannot clear it. Drive
    // it from a second spawned task so the check is task identity and not the
    // absent id of a `block_on` caller, which a guard rejecting only `None`
    // would also pass.
    let stale = tokio::spawn({
        let controller = Arc::clone(&controller);
        async move { controller.mark_child_loop_idle("child").await }
    });
    stale.await.expect("the stale bookkeeping task must finish");
    let published = {
        let state = controller.state.read().await;
        state
            .children
            .get("child")
            .expect("the record")
            .handle
            .as_ref()
            .map(tokio::task::JoinHandle::id)
    };
    assert_eq!(published, Some(replacement_id), "a loop that does not own the handle must leave it published");

    // The owning loop does retire it, so the guard above cannot make a handle
    // permanently uncloseable.
    gate.notify_one();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if controller
                .state
                .read()
                .await
                .children
                .get("child")
                .is_some_and(|record| record.handle.is_none())
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the owning loop must retire its own handle");
}
