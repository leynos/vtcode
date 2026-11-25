//! A child link published during a graft adopts the route the graft publishes.
//!
//! The graft and the link both mutating one controller's route is a separate
//! concern from the launch path, so its regression and its recording observer
//! live here, declared by `mod graft_publication;` from `tests.rs`.

use super::*;
use crate::core::threads::ThreadManager;
use crate::subagents::controller_tree::ChildRuntimePublication;
use crate::subagents::controller_tree::attachment_checkpoints::{AttachmentStage, arm};
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
