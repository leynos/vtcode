//! Settles a background record from the exec session its liveness read returned.

use super::*;

impl SubagentController {
    pub(super) async fn update_background_record_state(
        &self,
        record_id: &str,
        snapshot: Option<crate::tools::types::DiapasonExecSession>,
    ) -> Result<Option<(String, String, u8)>> {
        let mut state = self.state.write().await;
        let Some(record) = state.background_children.get_mut(record_id) else {
            return Ok(None);
        };
        // A settled record already carries its terminal outcome, and refresh
        // exists to settle records that are still active. Cancellation clears
        // `desired_enabled` while preserving an `Error` and its message, so
        // letting refresh rewrite it would relabel a failure as `Stopped` —
        // which clients project as "completed" — or relaunch it. Guarding here
        // covers every refresh arm at once.
        if !record.status.is_active() {
            return Ok(None);
        }
        record.updated_at = Utc::now();

        let Some(snapshot) = snapshot else {
            return Self::handle_missing_background_snapshot(record, &self.config);
        };

        record.pid = snapshot.child_pid;
        record.started_at = snapshot.started_at.or(record.started_at);

        match snapshot.lifecycle_state {
            Some(crate::tools::types::DiapasonSessionLifecycleState::Running) => {
                record.status = BackgroundSubprocessStatus::Running;
                record.ended_at = None;
                record.error = None;
            }
            Some(crate::tools::types::DiapasonSessionLifecycleState::Exited) | None => {
                record.ended_at.get_or_insert(Utc::now());
                if record.desired_enabled
                    && self.config.vt_cfg.subagents.background.auto_restore
                    && record.restart_attempts < 1
                {
                    let next_restart_attempt = record.restart_attempts.saturating_add(1);
                    record.restart_attempts = next_restart_attempt;
                    record.status = BackgroundSubprocessStatus::Starting;
                    tracing::warn!(
                        agent_name = record.agent_name.as_str(),
                        record_id = record.id.as_str(),
                        attempt = next_restart_attempt,
                        "Background subprocess exited unexpectedly; scheduling restart"
                    );
                    return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
                }
                Self::mark_background_record_stopped_or_error(record, &snapshot, &self.config);
            }
        }

        Ok(None)
    }

    fn handle_missing_background_snapshot(
        record: &mut BackgroundRecord,
        config: &SubagentControllerConfig,
    ) -> Result<Option<(String, String, u8)>> {
        if record.desired_enabled && config.vt_cfg.subagents.background.auto_restore {
            if record.restart_attempts < 1 {
                let next_restart_attempt = record.restart_attempts.saturating_add(1);
                record.restart_attempts = next_restart_attempt;
                record.status = BackgroundSubprocessStatus::Starting;
                tracing::warn!(
                    agent_name = record.agent_name.as_str(),
                    record_id = record.id.as_str(),
                    attempt = next_restart_attempt,
                    "Background subprocess is missing; scheduling restart"
                );
                return Ok(Some((record.agent_name.clone(), record.id.clone(), next_restart_attempt)));
            }
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some("Background subprocess is not running".to_string());
            record.ended_at.get_or_insert(Utc::now());
        } else if !record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.ended_at.get_or_insert(Utc::now());
        }
        Ok(None)
    }

    fn mark_background_record_stopped_or_error(
        record: &mut BackgroundRecord,
        snapshot: &crate::tools::types::DiapasonExecSession,
        _config: &SubagentControllerConfig,
    ) {
        if record.desired_enabled {
            record.status = BackgroundSubprocessStatus::Error;
            record.error = Some(match snapshot.exit_code {
                Some(exit_code) => format!("Background subprocess exited with code {exit_code}"),
                None => "Background subprocess exited unexpectedly".to_string(),
            });
        } else {
            record.status = BackgroundSubprocessStatus::Stopped;
        }
    }
}
