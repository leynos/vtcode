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
