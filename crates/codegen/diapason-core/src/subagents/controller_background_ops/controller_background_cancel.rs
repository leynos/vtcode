//! Graceful and forced cancellation of background subprocesses.

use super::*;

struct BackgroundCancellation {
    agent_name: String,
    exec_session_id: String,
    /// The record had already reached a terminal status, so cancellation left
    /// its outcome and timestamps untouched.
    already_terminal: bool,
}

enum BackgroundCancellationMode {
    Graceful,
    Force,
}

impl SubagentController {
    /// Gracefully stops a background subprocess by setting its desired state to disabled.
    pub async fn graceful_stop_background(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(target, BackgroundCancellationMode::Graceful).await
    }

    /// Force-cancels a background subprocess, closing its exec session immediately.
    pub async fn force_cancel_background(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(target, BackgroundCancellationMode::Force).await
    }

    async fn cancel_background(
        &self,
        target: &str,
        mode: BackgroundCancellationMode,
    ) -> Result<BackgroundSubprocessEntry> {
        let cancellation = self.begin_background_cancellation(target).await?;
        if cancellation.already_terminal {
            // A stopped or errored record is already settled. Preserve its
            // status, timestamps, and error text instead of rewriting it to
            // `Stopped` (which a transport would project as a successful
            // completion).
            // The cleared `desired_enabled` flag still has to reach disk so a
            // resumed session does not restore the cancelled record.
            self.save_background_state().await?;
            return self.background_status_for(target).await;
        }
        self.log_background_cancellation(target, &cancellation, &mode);
        if !cancellation.exec_session_id.is_empty() {
            match mode {
                BackgroundCancellationMode::Graceful => {
                    let _ = self.config.exec_sessions.terminate_session(&cancellation.exec_session_id).await;
                    let _ = self
                        .config
                        .exec_sessions
                        .prune_exited_session(&cancellation.exec_session_id)
                        .await;
                }
                BackgroundCancellationMode::Force => {
                    let _ = self.config.exec_sessions.close_session(&cancellation.exec_session_id).await;
                }
            }
        }
        self.finish_background_cancellation(target).await
    }

    fn log_background_cancellation(
        &self,
        target: &str,
        cancellation: &BackgroundCancellation,
        mode: &BackgroundCancellationMode,
    ) {
        let message = match mode {
            BackgroundCancellationMode::Graceful => "Gracefully stopping",
            BackgroundCancellationMode::Force => "Force cancelling",
        };
        tracing::info!(
            agent_name = cancellation.agent_name.as_str(),
            record_id = target,
            exec_session_id = cancellation.exec_session_id.as_str(),
            "{message} background subagent subprocess"
        );
    }

    async fn begin_background_cancellation(&self, target: &str) -> Result<BackgroundCancellation> {
        let mut state = self.state.write().await;
        let record = state
            .background_children
            .get_mut(target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        if current_subagent_invocation_context()
            .is_some_and(|context| context.parent_session_id != record.parent_session_id)
        {
            bail!("Unknown background subprocess {target}");
        }
        if !record.status.is_active() {
            // The record is already settled, so keep its status, timestamps,
            // and error text. The owner still asked to stop it, though, and
            // restore selection reads only `desired_enabled`, so clear the
            // flag to keep a settled record from being relaunched on resume.
            record.desired_enabled = false;
            record.updated_at = Utc::now();
            return Ok(BackgroundCancellation {
                agent_name: record.agent_name.clone(),
                exec_session_id: record.exec_session_id.clone(),
                already_terminal: true,
            });
        }
        record.desired_enabled = false;
        record.status = BackgroundSubprocessStatus::Stopped;
        record.updated_at = Utc::now();
        record.ended_at = Some(Utc::now());
        Ok(BackgroundCancellation {
            agent_name: record.agent_name.clone(),
            exec_session_id: record.exec_session_id.clone(),
            already_terminal: false,
        })
    }

    async fn finish_background_cancellation(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.refresh_background_archive_metadata(target).await?;
        self.save_background_state().await?;
        let status = self.background_status_for(target).await?;
        self.publish_background_progress(status.clone()).await;
        Ok(status)
    }
}
