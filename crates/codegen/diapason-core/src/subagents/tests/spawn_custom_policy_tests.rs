//! Custom spec spawn policy guards.
use super::*;

#[tokio::test]
async fn spawn_custom_rejects_write_capable_spec() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    let spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "worker")
        .expect("worker");

    let err = controller
        .spawn_custom(
            spec,
            SpawnAgentRequest {
                message: Some("Implement a change.".to_string()),
                ..SpawnAgentRequest::default()
            },
        )
        .await
        .expect_err("write-capable custom spec should be rejected");

    assert!(err.to_string().contains("custom subagent spawn only supports read-only specs"));
}

#[tokio::test]
async fn spawn_custom_rejects_primary_only_spec() {
    let temp = TempDir::new().expect("tempdir");
    write_test_primary_agent(temp.path());
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    let spec = controller
        .effective_specs()
        .await
        .into_iter()
        .find(|spec| spec.name == "duck")
        .expect("duck primary agent");

    let err = controller
        .spawn_custom(
            spec,
            SpawnAgentRequest {
                message: Some("Discuss the task.".to_string()),
                ..SpawnAgentRequest::default()
            },
        )
        .await
        .expect_err("primary-only custom spec should be rejected");

    assert!(
        err.to_string()
            .contains("custom subagent spawn only supports subagent-capable specs")
    );
}
