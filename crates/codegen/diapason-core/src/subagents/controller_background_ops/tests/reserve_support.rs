//! Fixtures shared by the reservation and replacement tests.

use crate::subagents::{BackgroundSubprocessStatus, SubagentController};

use super::super::BackgroundRunPlan;
use super::support::background_record;

pub(super) const WORKER: &str = "background-worker";
pub(super) const WORKER_AGENT: &str = "worker";
pub(super) const OBSERVED_EXEC: &str = "exec-observed";

/// Builds the worker spec the reservation tests launch.
pub(super) fn worker_spec() -> diapason_config::SubagentSpec {
    diapason_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == WORKER_AGENT)
        .expect("worker spec")
}

/// Builds a plan for one launch of the worker record.
pub(super) fn plan(
    spec: diapason_config::SubagentSpec,
    exec_session_id: &str,
    expected: Option<&str>,
) -> BackgroundRunPlan {
    BackgroundRunPlan {
        owner_session_id: None,
        spec,
        record_id: WORKER.to_string(),
        parent_session_id: "parent-session".to_string(),
        session_id: format!("session-{exec_session_id}"),
        exec_session_id: exec_session_id.to_string(),
        created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
        prompt: "work".to_string(),
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
        expected_exec_session_id: expected.map(str::to_owned),
    }
}

/// Builds one persisted row under an explicit canonical id.
pub(super) fn persisted_row(
    id: &str,
    owner_session_id: Option<&str>,
    agent_name: &str,
    exec_session_id: &str,
    desired_enabled: bool,
) -> crate::subagents::PersistedBackgroundRecord {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "owner_session_id": owner_session_id,
        "agent_name": agent_name,
        "display_label": agent_name,
        "description": agent_name,
        "source": "builtin",
        "color": null,
        "session_id": format!("session-{id}"),
        "parent_session_id": "parent-session",
        "exec_session_id": exec_session_id,
        "desired_enabled": desired_enabled,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": null,
        "ended_at": null,
        "pid": null,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": null,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    }))
    .expect("persisted background record")
}

/// Inserts the observed launch into the controller's state.
pub(super) async fn insert_observed_record(controller: &SubagentController) {
    let record =
        background_record(WORKER, None, WORKER_AGENT, OBSERVED_EXEC, BackgroundSubprocessStatus::Running, true);
    controller
        .state
        .write()
        .await
        .background_children
        .insert(record.id.clone(), record);
}

/// The ledger row a migrated launch is persisted under.
pub(super) fn owned_row_id(owner: &str) -> String {
    crate::subagents::background_record_id_for_owner(WORKER_AGENT, Some(owner))
}
