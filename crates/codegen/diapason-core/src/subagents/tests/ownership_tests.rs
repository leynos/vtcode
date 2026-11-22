//! Command-level ownership enforcement with no ambient invocation context.
//!
//! These cases call the controller commands directly, outside any
//! `with_subagent_invocation_context` scope. They therefore prove the ownership
//! decision comes from the explicit `TaskOwner` argument rather than a
//! task-local context that could be absent in another transport.
use super::*;

async fn controller_with_foreign_and_owned_children() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let spec = read_only_test_spec("ownership-child");
    {
        let mut state = controller.state.write().await;
        state.children.insert(
            "owned-child".to_string(),
            test_child_record(
                "owned-child",
                "owned-child-session",
                "root-session",
                &spec,
                SubagentStatus::Running,
                1,
                None,
            ),
        );
        state.children.insert(
            "foreign-child".to_string(),
            test_child_record(
                "foreign-child",
                "foreign-child-session",
                "other-session",
                &spec,
                SubagentStatus::Running,
                1,
                None,
            ),
        );
    }
    (temp, controller)
}

/// A session reads the task it started and nothing else.
#[tokio::test]
async fn a_session_reads_only_its_own_child_without_an_invocation_context() {
    let (_temp, controller) = controller_with_foreign_and_owned_children().await;

    let owned = controller
        .status_for(TaskOwner::Session("root-session"), "owned-child")
        .await
        .expect("the owning session reads its child");
    assert_eq!(owned.id, "owned-child");

    let error = controller
        .status_for(TaskOwner::Session("root-session"), "foreign-child")
        .await
        .expect_err("another session's child is foreign");
    assert!(error.to_string().contains("Unknown subagent id foreign-child"));
}

/// A snapshot of a foreign task fails before any runtime state is captured.
#[tokio::test]
async fn a_session_cannot_snapshot_a_foreign_child() {
    let (_temp, controller) = controller_with_foreign_and_owned_children().await;

    let error = controller
        .snapshot_for_thread(TaskOwner::Session("root-session"), "foreign-child")
        .await
        .expect_err("a foreign child has no readable snapshot");
    assert!(error.to_string().contains("Unknown subagent id foreign-child"));

    controller
        .snapshot_for_thread(TaskOwner::Session("root-session"), "owned-child")
        .await
        .expect("the owning session snapshots its child");
}

/// A foreign close is rejected without mutating the task it names.
#[tokio::test]
async fn a_foreign_close_leaves_the_child_untouched() {
    let (_temp, controller) = controller_with_foreign_and_owned_children().await;

    let error = controller
        .close(TaskOwner::Session("root-session"), "foreign-child")
        .await
        .expect_err("a foreign child cannot be closed");
    assert!(error.to_string().contains("Unknown subagent id foreign-child"));

    let untouched = controller
        .status_for(TaskOwner::Workspace, "foreign-child")
        .await
        .expect("the record still exists");
    assert_eq!(untouched.status, SubagentStatus::Running);
}

/// A grandchild lives in the child-scoped controller that spawned it, and that
/// controller cannot prove the root session's ancestry from its own snapshot.
///
/// The grandchild record names its parent's session, not the session that
/// started the delegation, and the child scope holds no link to its ancestors.
/// The transport therefore resolves the owned set from the root's tree and
/// presents it, which is what lets a nested management command reach a
/// grandchild; a set that omits the task is refused exactly like a foreign one.
#[tokio::test]
async fn a_resolved_tree_set_admits_a_grandchild_held_by_a_child_scope() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let spec = read_only_test_spec("ownership-grandchild");
    {
        let mut state = controller.state.write().await;
        state.children.insert(
            "grandchild".to_string(),
            test_child_record(
                "grandchild",
                "grandchild-session",
                "child-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }

    let error = controller
        .status_for(TaskOwner::Session("root-session"), "grandchild")
        .await
        .expect_err("a child scope cannot prove the root session's ancestry on its own");
    assert!(error.to_string().contains("Unknown subagent id grandchild"));

    let owned = std::collections::HashSet::from(["grandchild"]);
    controller
        .status_for(TaskOwner::SessionTree(&owned), "grandchild")
        .await
        .expect("the root session's resolved tree set admits its grandchild");

    let unrelated = std::collections::HashSet::from(["another-task"]);
    let error = controller
        .status_for(TaskOwner::SessionTree(&unrelated), "grandchild")
        .await
        .expect_err("a resolved set that omits the task must not admit it");
    assert!(error.to_string().contains("Unknown subagent id grandchild"));
}

/// The local workspace operator addresses any task the controller holds.
#[tokio::test]
async fn the_workspace_operator_reaches_every_child() {
    let (_temp, controller) = controller_with_foreign_and_owned_children().await;

    for target in ["owned-child", "foreign-child"] {
        let entry = controller
            .status_for(TaskOwner::Workspace, target)
            .await
            .expect("the workspace operator reads any child");
        assert_eq!(entry.id, target);
    }
}

async fn controller_with_ownerless_background() -> (TempDir, SubagentController) {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    let record = persisted_background_record_for_owner("background-worker", None);
    // The reader serves only a session this process launched; the fixture
    // record stands in for that launch.
    controller
        .config
        .exec_sessions
        .register_launched_task_session("session-background-worker");
    controller
        .state
        .write()
        .await
        .background_children
        .insert(record.id.clone(), BackgroundRecord::from_persisted(record));
    (temp, controller)
}

/// A record started before any session claimed it belongs to no session.
#[tokio::test]
async fn an_ownerless_background_record_is_invisible_to_every_session() {
    let (_temp, controller) = controller_with_ownerless_background().await;

    let error = controller
        .background_snapshot(TaskOwner::Session("root-session"), "background-worker")
        .await
        .expect_err("an ownerless record belongs to no session");
    assert!(error.to_string().contains("Unknown background subprocess background-worker"));

    let error = controller
        .background_output_tail(TaskOwner::Session("root-session"), "background-worker", 10)
        .await
        .expect_err("an ownerless record has no readable output for a session");
    assert!(error.to_string().contains("Unknown background subprocess background-worker"));

    controller
        .background_snapshot(TaskOwner::Workspace, "background-worker")
        .await
        .expect("the workspace operator still sees the record it started");
}
