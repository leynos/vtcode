//! Wire projection of controller records onto the Lody management transport.
//!
//! The controller owns runtime state; this module only shapes that state into
//! the camelCase task objects the Lody management RPCs return.

use diapason_core::subagents::{
    BackgroundSubprocessEntry, BackgroundSubprocessStatus, SubagentStatus, SubagentStatusEntry,
};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ManagedTask {
    pub(super) task_id: String,
    description: String,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    subagent_type: Option<String>,
    started_at_epoch_seconds: f64,
    ended_at_epoch_seconds: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stop_reason: Option<String>,
}

pub(super) fn managed_child_task(entry: &SubagentStatusEntry) -> ManagedTask {
    let status = match entry.status {
        SubagentStatus::Queued | SubagentStatus::Running | SubagentStatus::Waiting => "running",
        SubagentStatus::Completed => "completed",
        SubagentStatus::Failed => "failed",
        SubagentStatus::Closed => "killed",
    };
    ManagedTask {
        task_id: entry.id.clone(),
        description: entry.description.clone(),
        status,
        agent_id: Some(entry.session_id.clone()),
        subagent_type: Some(entry.agent_name.clone()),
        started_at_epoch_seconds: epoch_seconds(entry.created_at),
        ended_at_epoch_seconds: entry.completed_at.map(epoch_seconds),
        stop_reason: entry.error.clone(),
    }
}

pub(super) fn managed_background_task(entry: &BackgroundSubprocessEntry) -> ManagedTask {
    let status = match entry.status {
        BackgroundSubprocessStatus::Starting | BackgroundSubprocessStatus::Running => "running",
        BackgroundSubprocessStatus::Stopped => "completed",
        BackgroundSubprocessStatus::Error => "failed",
    };
    ManagedTask {
        task_id: entry.id.clone(),
        description: entry.description.clone(),
        status,
        agent_id: Some(entry.session_id.clone()),
        subagent_type: Some(entry.agent_name.clone()),
        started_at_epoch_seconds: epoch_seconds(entry.started_at.unwrap_or(entry.created_at)),
        ended_at_epoch_seconds: entry.ended_at.map(epoch_seconds),
        stop_reason: entry.error.clone(),
    }
}

pub(super) fn epoch_seconds(timestamp: chrono::DateTime<chrono::Utc>) -> f64 {
    timestamp.timestamp_millis().max(0) as f64 / 1_000.0
}

#[cfg(test)]
mod tests {
    //! Wire projection of every controller state onto the Lody task shape.
    use super::*;
    use serde_json::{Value, json};

    // Fixtures carry RFC 3339 instants; expectations use literal epoch seconds
    // so an assertion cannot pass by reusing the helper under test.
    const CREATED: &str = "2026-08-14T12:00:00Z";
    const STARTED: &str = "2026-08-14T12:00:30Z";
    const ENDED: &str = "2026-08-14T12:04:05Z";
    const CREATED_EPOCH_SECONDS: f64 = 1_786_708_800.0;
    const STARTED_EPOCH_SECONDS: f64 = 1_786_708_830.0;
    const ENDED_EPOCH_SECONDS: f64 = 1_786_709_045.0;

    fn child_entry(status: &str, completed_at: Option<&str>) -> SubagentStatusEntry {
        serde_json::from_value(json!({
            "id": "child-1",
            "session_id": "child-session",
            "parent_thread_id": "parent-session",
            "agent_name": "worker",
            "display_label": "Worker",
            "description": "Implement one bounded task",
            "source": "builtin",
            "status": status,
            "background": false,
            "depth": 1,
            "created_at": CREATED,
            "updated_at": CREATED,
            "completed_at": completed_at,
        }))
        .expect("a subagent record fixture")
    }

    fn background_entry(status: &str, started_at: Option<&str>, ended_at: Option<&str>) -> BackgroundSubprocessEntry {
        serde_json::from_value(json!({
            "id": "background-1",
            "session_id": "background-session",
            "exec_session_id": "exec-background-1",
            "agent_name": "watcher",
            "display_label": "Watcher",
            "description": "Watch the workspace",
            "source": "workspace",
            "status": status,
            "desired_enabled": true,
            "created_at": CREATED,
            "updated_at": CREATED,
            "started_at": started_at,
            "ended_at": ended_at,
        }))
        .expect("a background record fixture")
    }

    /// Every record state projects its own wire status, and both instants come
    /// from the record rather than from a shared default.
    #[test]
    fn management_projection_reports_a_status_and_independent_instants_per_state() {
        // (record status, wire status, completion instant)
        let child_cases = [
            ("queued", "running", None),
            ("running", "running", None),
            ("waiting", "running", None),
            ("completed", "completed", Some(ENDED)),
            ("failed", "failed", Some(ENDED)),
            ("closed", "killed", Some(ENDED)),
        ];
        for (record_status, wire_status, completed_at) in child_cases {
            let wire = serde_json::to_value(managed_child_task(&child_entry(record_status, completed_at)))
                .expect("serialize a managed child task");
            assert_eq!(wire["status"], wire_status, "a subagent {record_status} record projects its wire status");
            assert_eq!(
                wire["startedAtEpochSeconds"],
                json!(CREATED_EPOCH_SECONDS),
                "a subagent {record_status} record starts at its creation instant"
            );
            match completed_at {
                Some(_) => assert_eq!(
                    wire["endedAtEpochSeconds"],
                    json!(ENDED_EPOCH_SECONDS),
                    "a subagent {record_status} record ends at its completion instant"
                ),
                None => assert_eq!(
                    wire["endedAtEpochSeconds"],
                    Value::Null,
                    "an unfinished subagent {record_status} record reports no end instant"
                ),
            }
            assert_eq!(wire["taskId"], "child-1");
            assert_eq!(wire["agentId"], "child-session");
            assert_eq!(wire["subagentType"], "worker");
        }

        // (record status, wire status, start instant, end instant)
        let background_cases = [
            ("starting", "running", None, None),
            ("running", "running", Some(STARTED), None),
            ("stopped", "completed", Some(STARTED), Some(ENDED)),
            ("error", "failed", Some(STARTED), Some(ENDED)),
        ];
        for (record_status, wire_status, started_at, ended_at) in background_cases {
            let entry = background_entry(record_status, started_at, ended_at);
            let wire =
                serde_json::to_value(managed_background_task(&entry)).expect("serialize a managed background task");
            assert_eq!(wire["status"], wire_status, "a background {record_status} record projects its wire status");
            let expected_start = if started_at.is_some() {
                STARTED_EPOCH_SECONDS
            } else {
                CREATED_EPOCH_SECONDS
            };
            assert_eq!(
                wire["startedAtEpochSeconds"],
                json!(expected_start),
                "a background {record_status} record reports its recorded start, falling back to its creation"
            );
            match ended_at {
                Some(_) => assert_eq!(
                    wire["endedAtEpochSeconds"],
                    json!(ENDED_EPOCH_SECONDS),
                    "a background {record_status} record ends at its recorded end instant"
                ),
                None => assert_eq!(
                    wire["endedAtEpochSeconds"],
                    Value::Null,
                    "a running background {record_status} record reports no end instant"
                ),
            }
            assert_eq!(wire["taskId"], "background-1");
            assert_eq!(wire["agentId"], "background-session");
            assert_eq!(wire["subagentType"], "watcher");
        }
    }
}
