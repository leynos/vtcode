//! Shared fixtures for the background-liveness test modules.
//!
//! The record builders, the ledger writer and the projection helpers are used
//! by more than one of the focused modules below, so they live here instead of
//! being copied into each of them.
use super::*;
use crate::subagents::controller_background_ops::BackgroundRunPlan;

/// Builds a persisted record whose exec session is not registered with the
/// controller, so the liveness overlay applies, and overrides the cached
/// status and restart budget the projection consults.
pub(super) fn record_with_restart_state(
    id: &str,
    owner_session_id: Option<&str>,
    status: &str,
    restart_attempts: u8,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(persisted_background_record_for_owner(id, owner_session_id))
        .expect("serialize fixture record");
    // The archive reader authorizes a row by rebuilding its exec-session id
    // from its own session id, so the shared fixture carries a launch-shaped
    // pair rather than two unrelated identifiers.
    value["exec_session_id"] = serde_json::json!(format!("exec-session-{id}-1-1"));
    value["status"] = serde_json::json!(status);
    value["desired_enabled"] = serde_json::json!(true);
    value["restart_attempts"] = serde_json::json!(restart_attempts);
    // The base fixture describes an ended run; every record built here is
    // active by construction, and an active record must not carry an end
    // instant. `record_with_terminal_state` re-applies its explicit value.
    value["ended_at"] = serde_json::Value::Null;
    serde_json::from_value(value).expect("deserialize record")
}

/// Overrides the projection's terminal-time inputs on the fixture record:
/// whether the owner still desires the process, and whether a terminal instant
/// was ever observed.
pub(super) fn record_with_terminal_state(
    id: &str,
    owner_session_id: Option<&str>,
    status: &str,
    restart_attempts: u8,
    desired_enabled: bool,
    ended_at: Option<&str>,
) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(record_with_restart_state(id, owner_session_id, status, restart_attempts))
        .expect("serialize fixture record");
    value["desired_enabled"] = serde_json::json!(desired_enabled);
    value["ended_at"] = serde_json::json!(ended_at);
    serde_json::from_value(value).expect("deserialize record")
}

/// Builds the plan of a launch that has already been replaced or cancelled.
///
/// The plan names an exec session the stored record no longer carries, which
/// is exactly the state an older launch is in when it fails after a
/// cancellation settled the record or a replacement took its id.
pub(super) fn stale_launch_plan(exec_session_id: &str) -> BackgroundRunPlan {
    BackgroundRunPlan {
        owner_session_id: Some("session-a".to_string()),
        spec: diapason_config::builtin_subagents()
            .into_iter()
            .find(|spec| spec.name == "explorer")
            .expect("explorer spec"),
        record_id: "background-worker@session-a".to_string(),
        parent_session_id: "parent-session".to_string(),
        session_id: "child-session".to_string(),
        exec_session_id: exec_session_id.to_string(),
        created_at: Utc::now(),
        prompt: "work".to_string(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
        expected_exec_session_id: None,
    }
}

/// Resolves the ledger path for `workspace_root`, creating the state directory.
///
/// The ledger lives under the shared workspace-directory helper, which honours
/// an override, so building fixtures from a hard-coded `.diapason/state` path
/// would desynchronize them from the loader whenever a test or the harness
/// points the workspace directory somewhere else. Every fixture that writes the
/// ledger must therefore resolve it here.
pub(super) fn background_ledger_path(workspace_root: &std::path::Path) -> PathBuf {
    let path = background_state_path_for_tests(workspace_root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create the background state directory");
    }
    path
}

pub(super) fn write_ledger(path: &std::path::Path, records: Vec<PersistedBackgroundRecord>) {
    std::fs::write(path, serde_json::to_string(&PersistedBackgroundState { records }).expect("serialize state"))
        .expect("write state");
}

pub(super) fn auto_restore_config(workspace_root: PathBuf, auto_restore: bool) -> SubagentControllerConfig {
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.auto_restore = auto_restore;
    test_controller_config(workspace_root, cfg)
}

/// Projects one persisted record through the liveness query and returns the
/// entry the caller would observe.
pub(super) async fn projected_liveness_entry(
    temp: &TempDir,
    record: PersistedBackgroundRecord,
    auto_restore: bool,
) -> BackgroundSubprocessEntry {
    write_ledger(&background_ledger_path(temp.path()), vec![record]);
    let controller = SubagentController::new_with_background_owner(
        auto_restore_config(temp.path().to_path_buf(), auto_restore),
        Some("session-a"),
    )
    .await
    .expect("controller");

    controller
        .background_status_entries_with_liveness(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|observed| observed.entry.id == "background-worker@session-a")
        .expect("owned background record")
        .entry
}

/// Projects one persisted record through the liveness query and returns the
/// status the caller would observe.
pub(super) async fn projected_liveness_status(
    temp: &TempDir,
    record: PersistedBackgroundRecord,
    auto_restore: bool,
) -> BackgroundSubprocessStatus {
    projected_liveness_entry(temp, record, auto_restore).await.status
}

/// Rewrites the agent a persisted record names.
///
/// The agent is what decides the record's canonical id, so two fixture records
/// that keep the default agent join into one entry however distinct the ids
/// they were built with. A test that needs two records must therefore give
/// them different agents, not just different ids.
pub(super) fn with_agent_name(record: PersistedBackgroundRecord, agent_name: &str) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(record).expect("serialize fixture record");
    value["agent_name"] = serde_json::json!(agent_name);
    value["display_label"] = serde_json::json!(agent_name);
    value["description"] = serde_json::json!(agent_name);
    serde_json::from_value(value).expect("deserialize record")
}

/// Rewrites one persisted record with an explicit exec session.
pub(super) fn with_exec_session(record: PersistedBackgroundRecord, exec_session_id: &str) -> PersistedBackgroundRecord {
    let mut value = serde_json::to_value(record).expect("serialize fixture record");
    value["exec_session_id"] = serde_json::json!(exec_session_id);
    serde_json::from_value(value).expect("deserialize record")
}
