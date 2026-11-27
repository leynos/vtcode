//! Regressions for a launch or link that races a guard or a live loop.
//!
//! The graft window, a close that overtakes a parked launch, a resumed loop
//! claiming the record and a non-interrupt input arriving on a live run are
//! separate from the launch-path cases in `tests.rs`, so their regressions live
//! here, declared by `mod graft_publication;` from that module.

use super::*;
use crate::core::threads::ThreadManager;
use crate::subagents::controller_tree::ChildRuntimePublication;
use crate::subagents::controller_tree::attachment_checkpoints::{AttachmentStage, arm};
use crate::subagents::types::SendInputRequest;
use std::sync::Mutex;
use std::time::Duration;

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
    let leaf = {
        let mut leaf = SubagentController::new(nested_controller_config(workspace_root, "leaf-session"))
            .await
            .expect("leaf controller");
        // The production creator is `child_controller_for`, which inherits the
        // observers, the child-id scope and the lock scope as one unit. Building
        // the leaf any other way would let this regression pass while a scope
        // the production path also shares is left behind.
        leaf.inherit_tree_context(&middle, "middle-session").await;
        Arc::new(leaf)
    };
    let spec = explorer_spec();
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
    // Each post-release step is bounded on its own, so a graft or a publication
    // that cannot complete names itself instead of hanging the whole test.
    tokio::time::timeout(Duration::from_secs(5), graft)
        .await
        .expect("the graft must finish once the attachment guard is released")
        .expect("the graft task must not panic")
        .expect("the graft publishes");
    tokio::time::timeout(Duration::from_secs(5), publication)
        .await
        .expect("the child link must publish once the graft releases the guard")
        .expect("the child link publishes");

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

/// A launch overtaken by a close and a resume must not displace the live loop.
///
/// The launch publishes `Queued` and then releases the state lock before it
/// stores its handle, so a close can settle the record there and a resume can
/// reopen it and start its own loop. The stale launch then re-acquires the
/// lock with two possible views of the resumed loop: it has published its
/// handle but not yet dequeued its prompt (`Queued`), or it has dequeued and
/// is running (`Running`). Publishing the stale handle in either case would
/// replace the resumed one, and aborting that task would detach the run it had
/// already started, leaving the record with no loop left to settle it. The
/// launch must instead decline: exactly one loop owns the record, and a later
/// close aborts it.
#[tokio::test]
async fn a_launch_overtaken_by_a_close_and_resume_leaves_the_resumed_loop_alone() {
    for (resumed_status, dequeued) in [(SubagentStatus::Queued, false), (SubagentStatus::Running, true)] {
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

        // Park the first launch after it published the queued state and
        // released the lock, with no handle on the record yet.
        let (entered, release) = launch_checkpoints::arm(&controller.state);
        let overtaken = tokio::spawn({
            let controller = Arc::clone(&controller);
            async move { controller.launch_child("child").await }
        });
        tokio::time::timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("the launch must reach the checkpoint after publishing the queued status");

        // The close finds no handle to abort, so it settles the record.
        let closed = tokio::time::timeout(Duration::from_secs(5), controller.close(TaskOwner::Workspace, "child"))
            .await
            .expect("the close must not wait on the parked launch")
            .expect("closing the parked launch must succeed");
        assert_eq!(closed.status, SubagentStatus::Closed);

        // The resume reopens the record and its own loop claims the run. The
        // loop is a real task that stays alive, standing in for the resumed
        // run the resume's launch would start, so this test can see whether
        // the overtaken launch displaces it.
        assert!(
            controller
                .reopen_single("child")
                .await
                .expect("the resume must reopen the record")
        );
        let (resumed_alive_tx, mut resumed_alive_rx) = tokio::sync::oneshot::channel::<()>();
        let resumed = tokio::spawn(async move {
            let _alive = resumed_alive_tx;
            std::future::pending::<()>().await;
        });
        let resumed_id = resumed.id();
        {
            let mut state = controller.state.write().await;
            let record = state.children.get_mut("child").expect("the resumed record");
            record.handle = Some(resumed);
            if dequeued {
                record.dequeue_run().expect("the resumed loop dequeues its prompt");
            }
        }

        release.notify_one();
        overtaken
            .await
            .expect("the parked launch task must not panic")
            .expect("a launch that finds the resumed loop already owns the record has nothing to start");

        let published = {
            let state = controller.state.read().await;
            let record = state.children.get("child").expect("the resumed record");
            assert_eq!(record.status, resumed_status, "the overtaken launch must not change the resumed run");
            record.handle.as_ref().map(tokio::task::JoinHandle::id)
        };
        assert_eq!(published, Some(resumed_id), "the overtaken launch must leave the resumed loop's handle published");
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut resumed_alive_rx)
                .await
                .is_err(),
            "the overtaken launch must not abort the loop the resume started"
        );

        // A later close still finds and aborts the sole running loop.
        let closed = controller
            .close(TaskOwner::Workspace, "child")
            .await
            .expect("the later close must succeed");
        assert_eq!(closed.status, SubagentStatus::Closed);
        let aborted = tokio::time::timeout(Duration::from_secs(5), resumed_alive_rx).await;
        assert!(matches!(aborted, Ok(Err(_))), "the later close must abort the resumed loop, got {aborted:?}");
    }
}

/// A second non-interrupt input leaves a live child run alone.
///
/// `send_input` queues the first non-interrupt prompt and leaves the record
/// `Waiting` while its loop is still running. A second call used to fall
/// through to the restart arm, which marked the record `Queued` and relaunched;
/// the launch then displaced and aborted the loop mid-run. Input without
/// `interrupt` must only queue, so the live loop keeps the record and serves
/// both prompts.
#[tokio::test]
async fn a_second_non_interrupt_input_does_not_restart_a_live_child_run() {
    let temp = TempDir::new().expect("tempdir");
    let controller = Arc::new(
        SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller"),
    );
    let spec = explorer_spec();
    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Running, 1);
    let (alive_tx, mut alive_rx) = tokio::sync::oneshot::channel::<()>();
    record.handle = Some(tokio::spawn(async move {
        let _alive = alive_tx;
        std::future::pending::<()>().await;
    }));
    let live_handle = record.handle.as_ref().map(tokio::task::JoinHandle::id);
    controller.state.write().await.children.insert("child".to_string(), record);

    for prompt in ["first", "second"] {
        controller
            .send_input(
                TaskOwner::Workspace,
                SendInputRequest {
                    target: "child".to_string(),
                    message: Some(prompt.to_string()),
                    items: Vec::new(),
                    interrupt: false,
                },
            )
            .await
            .expect("a non-interrupt input queues on the live run");
    }

    let state = controller.state.read().await;
    let record = state.children.get("child").expect("the record");
    assert_eq!(
        record.handle.as_ref().map(tokio::task::JoinHandle::id),
        live_handle,
        "a non-interrupt input must leave the live loop's handle published"
    );
    assert_eq!(record.queued_prompts.len(), 2, "both prompts stay queued for the live loop");
    assert_eq!(record.status, SubagentStatus::Waiting, "the live run stays waiting, not restarted");
    drop(state);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut alive_rx).await.is_err(),
        "a non-interrupt input must not abort the live child run"
    );

    controller.signal_shutdown().await;
}

/// A launch must not abort a live loop that already took its prompt.
///
/// An interrupt that arrives before its launch publishes a handle leaves the
/// new prompt in the queue, and a loop that is already running can dequeue it
/// before the restart takes its first state lock. That loop then sets
/// `Running`. Overwriting `Running` with `Queued` and then publishing the
/// replacement aborted the loop that was running the prompt; the replacement
/// found an empty queue and exited, so the request returned success while the
/// record stayed `Queued` with nothing running it. The live loop owns the
/// prompt and must be left alone.
#[tokio::test]
async fn a_launch_does_not_abort_a_live_loop_that_already_took_the_prompt() {
    let temp = TempDir::new().expect("tempdir");
    let controller = Arc::new(
        SubagentController::new(test_controller_config(temp.path().to_path_buf()))
            .await
            .expect("controller"),
    );
    let spec = explorer_spec();
    let mut record = child_record("child", "parent-session", &spec, SubagentStatus::Running, 1);
    let (alive_tx, mut alive_rx) = tokio::sync::oneshot::channel::<()>();
    record.handle = Some(tokio::spawn(async move {
        let _alive = alive_tx;
        std::future::pending::<()>().await;
    }));
    let loop_id = record.handle.as_ref().map(tokio::task::JoinHandle::id);
    controller.state.write().await.children.insert("child".to_string(), record);

    controller
        .launch_child("child")
        .await
        .expect("a launch that finds a live loop declines without an error");

    let state = controller.state.read().await;
    let record = state.children.get("child").expect("the record");
    assert_eq!(
        record.status,
        SubagentStatus::Running,
        "the launcher must not overwrite the status of the loop that owns the prompt"
    );
    assert_eq!(
        record.handle.as_ref().map(tokio::task::JoinHandle::id),
        loop_id,
        "the launcher must leave the live loop's handle published"
    );
    drop(state);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut alive_rx).await.is_err(),
        "the launcher must not abort the loop that already took the prompt"
    );
    controller.signal_shutdown().await;
}
