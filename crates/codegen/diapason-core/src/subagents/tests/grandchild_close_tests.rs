//! Close cascades into child-scoped grandchildren.
use super::*;

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

#[tokio::test]
async fn close_does_not_affect_sibling_grandchildren() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");

    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");

    // Two siblings each with their own child-scoped controller and a grandchild.
    let controller_a =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller a");
    {
        let mut state = controller_a.state.write().await;
        state.children.insert(
            "a-grandchild".to_string(),
            test_child_record(
                "a-grandchild",
                "session-a-grandchild",
                "session-a",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }
    let controller_b =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller b");
    {
        let mut state = controller_b.state.write().await;
        state.children.insert(
            "b-grandchild".to_string(),
            test_child_record(
                "b-grandchild",
                "session-b-grandchild",
                "session-b",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }
    let controller_a = Arc::new(controller_a);
    let controller_b = Arc::new(controller_b);

    {
        let mut state = parent.state.write().await;
        state.children.insert(
            "a".to_string(),
            test_child_record(
                "a",
                "session-a",
                "parent-session",
                &spec,
                SubagentStatus::Running,
                1,
                Some(controller_a.clone()),
            ),
        );
        state.children.insert(
            "b".to_string(),
            test_child_record(
                "b",
                "session-b",
                "parent-session",
                &spec,
                SubagentStatus::Running,
                1,
                Some(controller_b.clone()),
            ),
        );
    }

    parent.close(TaskOwner::Workspace, "a").await.expect("close a");

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
