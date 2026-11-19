//! Resume reopens only the subtree it was asked to resume.
use super::*;

#[tokio::test]
async fn resume_restores_closed_grandchildren() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
        .await
        .expect("parent controller");

    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");

    // Child-scoped controller that has a grandchild under the child session id.
    let child_controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("child controller");
    let child_session_id = "child-session".to_string();
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

    // Closing the child cascades to the grandchild...
    parent.close("child").await.expect("close child");
    let closed_grandchild = child_controller.status_for("grandchild").await.expect("grandchild status");
    assert_eq!(closed_grandchild.status, SubagentStatus::Closed, "grandchild must be closed with the child");

    // ...and resuming the child must reopen the grandchild too, not just clear
    // the close gate.
    parent.resume("child").await.expect("resume child");
    let resumed_grandchild = child_controller.status_for("grandchild").await.expect("grandchild status");
    assert!(
        matches!(resumed_grandchild.status, SubagentStatus::Queued | SubagentStatus::Running),
        "resumed grandchild must be re-queued, got {:?}",
        resumed_grandchild.status
    );

    // Abort restarted child tasks so they cannot run provider work after the
    // temp workspace is removed.
    parent.signal_shutdown().await;
}

#[tokio::test]
async fn resume_does_not_affect_sibling_grandchildren() {
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

    // Close both siblings' grandchildren, then resume only "a": "b"'s
    // grandchild must stay closed (resume_tree must respect subtree isolation).
    parent.close("a").await.expect("close a");
    parent.close("b").await.expect("close b");

    parent.resume("a").await.expect("resume a");

    let a_grandchild = controller_a.status_for("a-grandchild").await.expect("a-grandchild status");
    assert!(
        matches!(a_grandchild.status, SubagentStatus::Queued | SubagentStatus::Running),
        "resumed 'a''s grandchild must be re-queued, got {:?}",
        a_grandchild.status
    );
    let b_grandchild = controller_b.status_for("b-grandchild").await.expect("b-grandchild status");
    assert_eq!(
        b_grandchild.status,
        SubagentStatus::Closed,
        "resuming sibling 'a' must not reopen sibling 'b''s grandchildren"
    );

    // Abort restarted child tasks so they cannot run provider work after the
    // temp workspace is removed.
    parent.signal_shutdown().await;
}
