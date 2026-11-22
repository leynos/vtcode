//! Regressions for the child launch path and for runtime publication.
//!
//! These cases pin one ordering contract: a record that was settled before a
//! launch or a publication reached the state lock must not be treated as if it
//! were still live, and a child link published during a graft must adopt the
//! route the graft published.

use super::*;
use crate::core::threads::ThreadManager;
use crate::subagents::controller_tree::ChildRuntimePublication;
use crate::subagents::controller_tree::attachment_checkpoints::{AttachmentStage, arm};
use crate::tools::registry::PtySessionManager;
use std::sync::Mutex;
use std::time::Duration;
use tempfile::TempDir;

/// Records the child snapshots a controller publishes through one observer key.
#[derive(Default)]
struct RecordingObserver {
    children: Mutex<Vec<SubagentStatusEntry>>,
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.children.lock().expect("children lock").push(entry);
    }

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
}

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
    // launch's re-check has to observe.
    let closed = controller.close(TaskOwner::Workspace, "child").await;
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

/// A child link published while this controller is being grafted adopts the
/// grafted route, and waits for the graft rather than publishing inside it.
///
/// A graft snapshots the subtree it will move before it adopts the parent
/// controller's route. A child link published in that window is not in the
/// snapshot, so unless the link re-adopts the route under the same attachment
/// guard, the child keeps publishing to the pre-graft registry and the tree's
/// observers never see its lifecycle updates.
#[tokio::test]
async fn a_child_link_published_during_a_graft_adopts_the_grafted_route() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let scope = Arc::new(SubagentLockScope::new());
    let mut root_config = test_controller_config(workspace_root.clone());
    root_config.lock_scope = Some(Arc::clone(&scope));
    let root = Arc::new(SubagentController::new(root_config).await.expect("root controller"));
    let mut middle_config = nested_controller_config(workspace_root.clone(), "middle-session");
    middle_config.lock_scope = Some(Arc::clone(&scope));
    let middle = Arc::new(SubagentController::new(middle_config).await.expect("middle controller"));
    let leaf = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "leaf-session"))
            .await
            .expect("leaf controller"),
    );
    let spec = explorer_spec();

    // The leaf controller inherited the middle controller's pre-graft route,
    // exactly as a nested controller created by `child_controller_for` does.
    leaf.inherit_progress_observers(&middle, "middle-session").await;
    middle
        .state
        .write()
        .await
        .children
        .insert("leaf".to_string(), child_record("leaf", "middle-session", &spec, SubagentStatus::Running, 2));
    root.state
        .write()
        .await
        .children
        .insert("middle".to_string(), child_record("middle", "parent-session", &spec, SubagentStatus::Running, 1));

    // Park the graft immediately before it publishes the link, after it has
    // snapshotted the subtree and adopted its route: the child link below is
    // therefore in exactly the window that would otherwise strand it.
    let (entered, release) = arm(&root.state, AttachmentStage::BeforePublication);
    let graft = tokio::spawn({
        let root = Arc::clone(&root);
        let middle = Arc::clone(&middle);
        async move { root.attach_child_controller("middle", middle).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the graft must reach the checkpoint immediately before it publishes");

    let mut publication = std::pin::pin!(middle.publish_child_runtime_guarded(
        "leaf",
        ChildRuntimePublication {
            archive_metadata: build_subagent_archive_metadata(
                Path::new("/workspace"),
                "test-model",
                "openai",
                "default",
                "medium",
                "middle-session",
                false,
            ),
            archive_path: PathBuf::from("leaf-archive.json"),
            effective_config: DiapasonConfig::default(),
            thread_handle: ThreadManager::new().start_thread_with_identifier("leaf", ThreadBootstrap::new(None)),
            child_controller: Some(Arc::clone(&leaf)),
        },
        "middle-session",
    ));
    let blocked = tokio::time::timeout(Duration::from_millis(100), &mut publication).await;
    assert!(blocked.is_err(), "the child link must wait for the attachment guard the graft holds");

    release.notify_one();
    graft
        .await
        .expect("the graft task must not panic")
        .expect("the graft publishes");
    publication
        .await
        .expect("the child link publishes once the graft releases the guard");

    let observer = Arc::new(RecordingObserver::default());
    let _registration = root.register_progress_observer("parent-session", observer.clone());

    let record = child_record("leaf-task", "leaf-session", &spec, SubagentStatus::Running, 3);
    leaf.state.write().await.children.insert("leaf-task".to_string(), record);
    leaf.publish_child_status("leaf-task").await;

    let observed = observer.children.lock().expect("children lock").clone();
    let ids = observed.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, ["leaf-task"], "a child linked during the graft must publish through the tree observer");

    // Shutdown publishes terminal snapshots into the same observer, so the
    // assertion above must release the lock first.
    root.signal_shutdown().await;
    middle.signal_shutdown().await;
    leaf.signal_shutdown().await;
}
