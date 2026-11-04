//! Verifies custom subagent spawning preserves the selected specification despite other agent mentions.
use super::*;

#[tokio::test]
async fn spawn_custom_preserves_explicit_spec_when_input_mentions_multiple_agents() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
            .await
            .expect("controller");

    let mut spec = diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer");
    spec.name = "init-grounding-explorer".to_string();
    spec.description = "Diapason /init grounding explorer.".to_string();
    spec.source = SubagentSource::ProjectDiapason;

    let mentions = controller
        .set_turn_delegation_hints_from_input("@agent-explorer and @agent-worker inspect the repository")
        .await;
    assert_eq!(mentions, ["explorer", "worker"]);

    let spawned = controller
        .spawn_custom(
            spec,
            SpawnAgentRequest {
                message: Some("Inspect the repository and report agent-facing findings.".to_string()),
                max_turns: Some(2),
                ..SpawnAgentRequest::default()
            },
        )
        .await
        .expect("spawn");

    assert_eq!(spawned.agent_name, "init-grounding-explorer");
    assert_eq!(spawned.source, SubagentSource::ProjectDiapason.label());
    controller.close(&spawned.id).await.expect("close");
}
