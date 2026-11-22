//! Spawn and resume gates while a controller is closing or shut down.
use super::*;

#[tokio::test]
async fn spawn_is_rejected_after_close_begins() {
    let temp = TempDir::new().expect("tempdir");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.max_depth = 2;
    let mut child_config = test_controller_config(temp.path().to_path_buf(), vt_cfg);
    child_config.depth = 1;
    let child_controller = SubagentController::new(child_config).await.expect("child controller");
    child_controller
        .set_turn_delegation_hints_from_input("delegate this task")
        .await;

    // Mark the controller as closing: this is exactly what `close_tree` does
    // to a child-scoped controller before aborting its subtree, so a still-
    // running child cannot spawn a grandchild after the descendant snapshot.
    child_controller.begin_close().await;

    let err = child_controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Inspect the codebase.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect_err("spawn after close began must be rejected");

    assert!(err.to_string().contains("shutting down"));

    // Reopening the subtree clears the transient close flag, so a resumed
    // child can delegate again. Permanent `shutdown_requested` stays set only
    // for real shutdown, not for a subtree close.
    child_controller.end_close().await;
    child_controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("explorer".to_string()),
            message: Some("Inspect the codebase.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn after end_close must be allowed again");

    // Abort the queued child task so it cannot run provider work after the
    // temp workspace is removed.
    child_controller.signal_shutdown().await;
}

#[tokio::test]
async fn resume_rejected_after_shutdown() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    controller.signal_shutdown().await;

    let err = controller
        .resume(TaskOwner::Workspace, "some-agent")
        .await
        .expect_err("resume after shutdown must be rejected");

    assert!(err.to_string().contains("shutting down"));
}
