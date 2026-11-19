//! Manages background subagent work, including task completion, cancellation, and archived results.

mod controller_background_cancel;
mod controller_background_launch;
mod controller_background_lifecycle;
mod controller_background_liveness;
mod controller_background_query;
mod controller_background_run;
mod controller_background_settlement;

#[cfg(test)]
mod tests;

use super::*;

use self::controller_background_liveness::exec_session_is_missing;
use self::controller_background_query::{read_output_tail, read_preview};

pub(super) struct BackgroundRunRequest {
    pub(super) agent_name: String,
    pub(super) stable_id: Option<String>,
    pub(super) restart_attempts: u8,
    pub(super) overrides: Option<BackgroundLaunchOverrides>,
}

/// The identity one background launch was prepared with.
///
/// `pub(super)` rather than private so the controller's tests can drive the
/// settlement path with a launch that has already been replaced, which is the
/// only way to exercise the ownership guard without racing a real spawn.
pub(super) struct BackgroundRunPlan {
    pub(super) owner_session_id: Option<String>,
    pub(super) spec: SubagentSpec,
    pub(super) record_id: String,
    pub(super) parent_session_id: String,
    pub(super) session_id: String,
    pub(super) exec_session_id: String,
    pub(super) created_at: chrono::DateTime<Utc>,
    pub(super) prompt: String,
    pub(super) max_turns: Option<usize>,
    pub(super) model_override: Option<String>,
    pub(super) reasoning_override: Option<String>,
    pub(super) restart_attempts: u8,
}

fn usable_background_parent_session_id(owner: Option<String>) -> Option<String> {
    owner.filter(|owner| !owner.is_empty())
}

/// Reports whether a failed launch still owns the record it wrote.
///
/// A background record id is stable across restarts and shared with the
/// workspace ledger, so the record a launch wrote at `Starting` may no longer
/// describe that launch. A cancellation settles it with `desired_enabled`
/// cleared, and a replacement launch takes the id with a new exec session. An
/// older launch that fails afterwards must not settle either: marking a live
/// replacement as `Error` also makes a later cancellation take the
/// already-terminal branch and skip process termination. The exec session
/// identifies the launch, so only the matching, still-desired, still-starting
/// record may be settled.
fn failed_launch_still_owns_record(record: &BackgroundRecord, plan_exec_session_id: &str) -> bool {
    record.exec_session_id == plan_exec_session_id
        && matches!(record.status, BackgroundSubprocessStatus::Starting)
        && record.desired_enabled
}

impl SubagentController {
    /// Settles a background record whose exec session could not be spawned
    /// once no automatic restart remains.
    ///
    /// A record left at `Starting` is reported as an active process forever:
    /// refresh treats `Starting` as active, and the read-only liveness overlay
    /// preserves it while the launch is still desired. Settling it here makes
    /// the failure visible to a management list and keeps the default toggle
    /// from routing a graceful stop to a process that never started. A launch
    /// that still has its restart allowance is left `Starting`, matching the
    /// overlay's restart budget, and the original spawn error is returned
    /// either way.
    ///
    /// Only the launch that failed may settle the record. A record id outlives
    /// one launch: a cancellation can settle the record while its exec session
    /// is still starting, and a replacement launch can take the same id. An
    /// older launch that fails afterwards must not mark the replacement's live
    /// process as failed, because a later cancellation would then take the
    /// already-terminal branch and skip process termination.
    pub(super) async fn settle_failed_background_run(&self, plan: &BackgroundRunPlan, error: &anyhow::Error) {
        {
            let mut state = self.state.write().await;
            let Some(record) = state.background_children.get_mut(&plan.record_id) else {
                return;
            };
            if !failed_launch_still_owns_record(record, plan.exec_session_id.as_str()) {
                return;
            }
            if self.config.vt_cfg.subagents.background.auto_restore && record.restart_attempts < 1 {
                return;
            }
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some(format!("Failed to start background subagent '{}': {error:#}", record.agent_name));
            record.summary = Some("Background subagent failed to start".to_string());
            record.updated_at = Utc::now();
            record.ended_at.get_or_insert(Utc::now());
        }
        self.publish_background_status(&plan.record_id).await;
        // The caller receives the original spawn error, so a persistence
        // failure must not replace it; the ledger catches up on the next save.
        if let Err(save_error) = self.save_background_state().await {
            tracing::warn!(
                record_id = plan.record_id.as_str(),
                error = %save_error,
                "Failed to persist the settled background launch failure"
            );
        }
    }
}
