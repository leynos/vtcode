//! Runtime model overrides across spawn, resume and snapshot capture.
use super::*;

#[tokio::test]
async fn spawn_honours_model_override_when_user_explicitly_requests_it() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    controller
        .set_turn_delegation_hints_from_input("delegate this task using gpt-5.4-mini")
        .await;

    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement the change.".to_string()),
            model: Some(models::openai::GPT_5_6_LUNA.to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn");

    let effective_model = wait_for_effective_model(&controller, &spawned.id)
        .await
        .expect("effective model");
    assert_eq!(effective_model, models::openai::GPT_5_6_LUNA);
    controller.close(&spawned.id).await.expect("close");
}

#[tokio::test]
async fn resume_preserves_captured_runtime_overrides() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    controller
        .set_turn_delegation_hints_from_input("delegate this task using gpt-5.4-mini")
        .await;

    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement the change.".to_string()),
            model: Some(models::openai::GPT_5_6_LUNA.to_string()),
            max_turns: Some(2),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn");

    let initial_model = wait_for_effective_model(&controller, &spawned.id)
        .await
        .expect("initial effective model");
    assert_eq!(initial_model, models::openai::GPT_5_6_LUNA);

    let closed = controller.close(&spawned.id).await.expect("close");
    assert_eq!(closed.status, SubagentStatus::Closed);

    controller.resume(&spawned.id).await.expect("resume");

    for _ in 0..100 {
        let status = controller.status_for(&spawned.id).await.expect("status");
        if status.updated_at > closed.updated_at && status.status != SubagentStatus::Closed {
            let snapshot = controller.snapshot_for_thread(&spawned.id).await.expect("snapshot");
            assert_eq!(snapshot.effective_config.agent.default_model, models::openai::GPT_5_6_LUNA);
            controller.close(&spawned.id).await.expect("final close");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    panic!("resumed subagent did not capture runtime config in time");
}

#[tokio::test]
async fn spawn_captures_runtime_config_before_first_child_turn() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let spawned = controller
        .spawn(SpawnAgentRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement the change.".to_string()),
            ..SpawnAgentRequest::default()
        })
        .await
        .expect("spawn");

    let snapshot = controller.snapshot_for_thread(&spawned.id).await.expect("snapshot");

    assert_eq!(snapshot.id, spawned.id);
    assert!(!snapshot.effective_config.agent.default_model.trim().is_empty());

    controller.close(&spawned.id).await.expect("close");
}
