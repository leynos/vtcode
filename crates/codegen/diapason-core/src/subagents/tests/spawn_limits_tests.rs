//! Concurrency, depth and worktree gates for spawn.
use super::*;

#[tokio::test]
async fn spawn_rejects_fourth_active_subagent() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");

    {
        let mut state = controller.state.write().await;
        for idx in 0..SUBAGENT_HARD_CONCURRENCY_LIMIT {
            let id = format!("active-{idx}");
            state.children.insert(
                id.clone(),
                test_child_record(
                    &id,
                    &format!("session-{id}"),
                    "parent-session",
                    &spec,
                    SubagentStatus::Running,
                    1,
                    None,
                ),
            );
        }
    }

    let err = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Inspect another codepath.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect_err("fourth active subagent should be rejected");

    assert!(err.to_string().contains(&format!(
            "Subagent concurrency limit reached (max_concurrent={})",
            controller.config.vt_cfg.subagents.max_concurrent.min(
                SUBAGENT_HARD_CONCURRENCY_LIMIT
            )
        )));
}

#[tokio::test]
async fn spawn_from_child_controller_respects_depth_limit() {
    let temp = TempDir::new().expect("tempdir");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.max_depth = 2;

    // Child controller runs at depth 1: it may spawn a grandchild (depth 2),
    // but the grandchild itself (depth 2) may not spawn further.
    let mut child_config = test_controller_config(temp.path().to_path_buf(), vt_cfg.clone());
    child_config.depth = 1;
    let child_controller = SubagentController::new(child_config).await.expect("child controller");

    child_controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Inspect the codebase.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("grandchild spawn should be allowed at depth 1 with max_depth=2");

    // Now a controller at depth 2 must refuse another spawn.
    let mut grandchild_config = test_controller_config(temp.path().to_path_buf(), vt_cfg);
    grandchild_config.depth = 2;
    let grandchild_controller = SubagentController::new(grandchild_config).await.expect("grandchild controller");

    let err = grandchild_controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Inspect yet more.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect_err("spawn at depth == max_depth should hit the depth limit");

    assert!(err.to_string().contains("Subagent depth limit reached (max_depth=2)"));

    // Abort queued child tasks so they cannot run provider work after the
    // temp workspace is removed.
    child_controller.signal_shutdown().await;
    grandchild_controller.signal_shutdown().await;
}

#[tokio::test]
async fn spawn_at_capacity_does_not_create_an_isolation_worktree() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");
    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let spec = {
        let mut state = controller.state.write().await;
        let spec = state
            .discovered
            .effective
            .iter_mut()
            .find(|spec| spec.name == "worker")
            .expect("worker");
        spec.isolation = Some(IsolationMode::Worktree);
        spec.clone()
    };

    {
        let mut state = controller.state.write().await;
        for idx in 0..SUBAGENT_HARD_CONCURRENCY_LIMIT {
            let id = format!("active-{idx}");
            state.children.insert(
                id.clone(),
                test_child_record(
                    &id,
                    &format!("session-{id}"),
                    "parent-session",
                    &spec,
                    SubagentStatus::Running,
                    1,
                    None,
                ),
            );
        }
    }

    let err = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement a change.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect_err("spawn at capacity should be rejected before any worktree is created");

    assert!(err.to_string().contains("Subagent concurrency limit reached"));
    let worktrees_dir = temp.path().join(diapason_commons::workspace_dir_name()).join("worktrees");
    let created = std::fs::read_dir(&worktrees_dir).map(|entries| entries.count()).unwrap_or(0);
    assert_eq!(created, 0, "a rejected spawn must not leave a worktree behind");
}

#[tokio::test]
async fn nested_spawn_rejects_worktree_isolation() {
    let temp = TempDir::new().expect("tempdir");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.max_depth = 2;
    let mut child_config = test_controller_config(temp.path().to_path_buf(), vt_cfg);
    child_config.depth = 1;
    let child_controller = SubagentController::new(child_config).await.expect("child controller");
    child_controller
        .set_turn_delegation_hints_from_input("delegate this task")
        .await;

    // Force the discovered "worker" spec to request worktree isolation so the
    // nested-spawn guard can be exercised.
    {
        let mut state = child_controller.state.write().await;
        if let Some(spec) = state.discovered.effective.iter_mut().find(|spec| spec.name == "worker") {
            spec.isolation = Some(IsolationMode::Worktree);
        }
    }

    let err = child_controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement a change.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect_err("nested worktree isolation should be rejected");

    assert!(err.to_string().contains("nested worktree isolation is not supported"));
}
