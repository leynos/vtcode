//! Regressions for nested-controller discovery and settled-outcome preservation.
use super::*;

mod background_restoration;
mod observer_routing;
mod task_id_adoption;
mod task_id_allocation;

fn explorer_spec() -> SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer spec")
}

fn settled_background_record(
    id: &str,
    owner_session_id: &str,
    status: BackgroundSubprocessStatus,
    error: Option<&str>,
) -> BackgroundRecord {
    let timestamp = Utc::now();
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: Some(owner_session_id.to_string()),
        agent_name: "watcher".to_string(),
        display_label: "Watcher".to_string(),
        description: "Watch the workspace".to_string(),
        source: "workspace".to_string(),
        colour: None,
        session_id: format!("session-{id}"),
        parent_session_id: owner_session_id.to_string(),
        exec_session_id: format!("exec-{id}"),
        desired_enabled: false,
        status,
        created_at: timestamp,
        updated_at: timestamp,
        started_at: Some(timestamp),
        ended_at: Some(timestamp),
        pid: Some(4242),
        prompt: "Watch the workspace".to_string(),
        summary: Some("settled".to_string()),
        error: error.map(str::to_string),
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
    }
}

async fn nested_child_controller(workspace_root: PathBuf, parent_session_id: &str) -> Arc<SubagentController> {
    let mut config = test_controller_config(workspace_root, DiapasonConfig::default());
    config.parent_session_id = parent_session_id.to_string();
    config.depth = 1;
    // Mirror `child_controller_for`: nested scopes run under the managed
    // background runtime guard and therefore never own background work.
    config.managed_background_runtime = true;
    Arc::new(SubagentController::new(config).await.expect("nested controller"))
}
