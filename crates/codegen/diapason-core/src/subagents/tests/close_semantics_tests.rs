//! Close semantics for settled, active and nested children.
use super::*;

#[tokio::test]
async fn close_marks_child_closed() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;
    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("default".to_string()),
            message: Some("Summarize the repository.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn");
    let closed = controller.close(TaskOwner::Workspace, &spawned.id).await.expect("close");
    assert_eq!(closed.status, SubagentStatus::Closed);
}

#[tokio::test]
async fn close_is_idempotent_for_closed_agents() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;
    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("default".to_string()),
            message: Some("Summarize the repository.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn");

    let closed = controller.close(TaskOwner::Workspace, &spawned.id).await.expect("first close");
    let closed_again = controller.close(TaskOwner::Workspace, &spawned.id).await.expect("second close");

    assert_eq!(closed_again.status, SubagentStatus::Closed);
    assert_eq!(closed_again.updated_at, closed.updated_at);
    assert_eq!(closed_again.completed_at, closed.completed_at);
}

#[tokio::test]
async fn close_and_resume_cascade_through_spawn_tree() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");

    {
        let mut state = controller.state.write().await;
        state.children.insert(
            "parent".to_string(),
            test_child_record("parent", "session-parent", "session-root", &spec, SubagentStatus::Running, 1, None),
        );
        state.children.insert(
            "child".to_string(),
            test_child_record("child", "session-child", "parent", &spec, SubagentStatus::Running, 2, None),
        );
        state.children.insert(
            "grandchild".to_string(),
            test_child_record("grandchild", "session-grandchild", "child", &spec, SubagentStatus::Running, 3, None),
        );
    }

    let closed = controller.close(TaskOwner::Workspace, "parent").await.expect("close");
    assert_eq!(closed.status, SubagentStatus::Closed);
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "child")
            .await
            .expect("child")
            .status,
        SubagentStatus::Closed
    );
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "grandchild")
            .await
            .expect("grandchild")
            .status,
        SubagentStatus::Closed
    );

    let subtree_ids = controller.collect_spawn_subtree_ids("parent").await.expect("collect subtree");
    assert_eq!(subtree_ids, vec!["parent".to_string(), "child".to_string(), "grandchild".to_string()]);

    let mut restart_ids = Vec::new();
    for node_id in subtree_ids {
        if controller.reopen_single(node_id.as_str()).await.expect("reopen subtree node") {
            restart_ids.push(node_id);
        }
    }

    assert_eq!(restart_ids, vec!["parent".to_string(), "child".to_string(), "grandchild".to_string()]);
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "parent")
            .await
            .expect("parent")
            .status,
        SubagentStatus::Queued
    );
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "child")
            .await
            .expect("child")
            .status,
        SubagentStatus::Queued
    );
    assert_eq!(
        controller
            .status_for(TaskOwner::Workspace, "grandchild")
            .await
            .expect("grandchild")
            .status,
        SubagentStatus::Queued
    );
}
