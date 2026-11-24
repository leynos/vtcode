//! Close cascades into child-scoped grandchildren.
use super::*;
use rstest::rstest;

#[tokio::test]
async fn close_cascades_to_child_scoped_grandchildren() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");

    // Child-scoped controller that already has a grandchild tracked under the
    // child's session id.
    let child_controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("child controller");
    let child_session_id = "child-session".to_string();
    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");
    {
        let mut state = child_controller.state.write().await;
        state.children.insert(
            "grandchild".to_string(),
            test_child_record(
                "grandchild",
                "session-grandchild",
                &child_session_id,
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }
    let child_controller = Arc::new(child_controller);

    // Register the child on the parent controller with the child-scoped
    // controller attached, then close it and assert the grandchild is closed.
    {
        let mut state = parent.state.write().await;
        state.children.insert(
            "child".to_string(),
            test_child_record(
                "child",
                &child_session_id,
                "parent-session",
                &spec,
                SubagentStatus::Running,
                1,
                Some(child_controller.clone()),
            ),
        );
    }

    let closed = parent.close(TaskOwner::Workspace, "child").await.expect("close child");
    assert!(closed.status.is_terminal());

    let grandchild_status = child_controller
        .status_for(TaskOwner::Workspace, "grandchild")
        .await
        .expect("grandchild status");
    assert_eq!(
        grandchild_status.status,
        SubagentStatus::Closed,
        "closing the child must cascade to its grandchildren"
    );
}

#[rstest]
#[tokio::test]
async fn close_does_not_affect_sibling_grandchildren(#[future] sibling_controllers: SiblingControllers) {
    let SiblingControllers { _temp, parent, controller_a, controller_b } = sibling_controllers.await;

    parent.close(TaskOwner::Workspace, "a").await.expect("close a");

    // A status read alone cannot prove 'b' stayed open: closing it marks the
    // controller itself, which only a spawn admission observes. This module is
    // a descendant of the controller's own, so read the close flag directly and
    // assert the controller that owns 'b' was left open to delegation.
    assert!(
        !controller_b.closing.load(Ordering::Relaxed),
        "closing sibling 'a' must leave sibling 'b' open to delegation"
    );

    let b_grandchild = controller_b
        .status_for(TaskOwner::Workspace, "b-grandchild")
        .await
        .expect("b-grandchild status");
    assert_eq!(
        b_grandchild.status,
        SubagentStatus::Running,
        "closing sibling 'a' must not affect sibling 'b''s grandchildren"
    );
    let a_grandchild = controller_a
        .status_for(TaskOwner::Workspace, "a-grandchild")
        .await
        .expect("a-grandchild status");
    assert_eq!(a_grandchild.status, SubagentStatus::Closed, "closing 'a' must cascade to its own grandchildren");
}
