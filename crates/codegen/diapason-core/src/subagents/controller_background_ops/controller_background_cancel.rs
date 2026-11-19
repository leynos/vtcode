//! Graceful and forced cancellation of background subprocesses.

use super::*;

use super::controller_background_liveness::exec_session_is_missing;
use super::controller_background_query::resolve_background_target_mut;

struct BackgroundCancellation {
    /// The canonical id the record is stored under.
    ///
    /// The caller may have addressed the record by a legacy id, so every later
    /// step uses the resolved id rather than the raw target.
    record_id: String,
    agent_name: String,
    exec_session_id: String,
    /// The record had already reached a terminal status, so cancellation left
    /// its outcome and timestamps untouched.
    already_terminal: bool,
    /// The record was already not desired when this cancellation began.
    ///
    /// Its terminal state therefore came from an earlier stop: either a retry
    /// of a cancellation whose save failed, or a stop another controller
    /// persisted and this controller adopted. In both cases the recorded exec
    /// session may still be alive, because the controller that wrote the stop
    /// may not have been able to see or close it.
    stop_already_recorded: bool,
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
            //
            // A forced cancellation must still close whatever exec session the
            // record names: a graceful stop marks the record `Stopped` before
            // the process leaves, and pruning only removes a session that has
            // already exited, so the session can outlive the stop. A record
            // whose stop was already recorded has the same problem for either
            // mode: the controller that recorded it may not have owned the
            // session, so this controller is the one that can terminate it.
            if !cancellation.exec_session_id.is_empty()
                && (matches!(mode, BackgroundCancellationMode::Force) || cancellation.stop_already_recorded)
            {
                self.close_background_exec_session(&cancellation.exec_session_id).await;
            }
            // The cleared `desired_enabled` flag still has to reach disk so a
            // resumed session does not restore the cancelled record. The save
            // comes first: a first attempt can settle the record and then fail
            // here, leaving this retry as the only publisher of the terminal
            // state, so the snapshot is published only once it is committed.
            self.save_background_state().await?;
            let status = self.background_status_for(&cancellation.record_id).await?;
            self.publish_background_progress(status.clone()).await;
            return Ok(status);
        }
        self.log_background_cancellation(&cancellation, &mode);
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
                    self.close_background_exec_session(&cancellation.exec_session_id).await;
                }
            }
        }
        self.finish_background_cancellation(&cancellation.record_id).await
    }

    /// Closes a background exec session, tolerating one that is already gone.
    ///
    /// A cancellation is idempotent: the session may have exited on its own or
    /// been closed by an earlier attempt. A typed not-found cause is therefore
    /// the expected outcome, not a failure to report. Any other fault is logged
    /// and then ignored, because the record's terminal state is already the
    /// caller's answer and must not be replaced by a cleanup error.
    pub(super) async fn close_background_exec_session(&self, exec_session_id: &str) {
        if let Err(error) = self.config.exec_sessions.close_session(exec_session_id).await
            && !exec_session_is_missing(&error)
        {
            tracing::warn!(
                exec_session_id,
                error = %error,
                "Failed to close the background exec session"
            );
        }
    }

    fn log_background_cancellation(&self, cancellation: &BackgroundCancellation, mode: &BackgroundCancellationMode) {
        let message = match mode {
            BackgroundCancellationMode::Graceful => "Gracefully stopping",
            BackgroundCancellationMode::Force => "Force cancelling",
        };
        tracing::info!(
            agent_name = cancellation.agent_name.as_str(),
            record_id = cancellation.record_id.as_str(),
            exec_session_id = cancellation.exec_session_id.as_str(),
            "{message} background subagent subprocess"
        );
    }

    async fn begin_background_cancellation(&self, target: &str) -> Result<BackgroundCancellation> {
        let mut state = self.state.write().await;
        let record = resolve_background_target_mut(&mut state, target)
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
            let stop_already_recorded = !record.desired_enabled;
            record.desired_enabled = false;
            record.updated_at = Utc::now();
            return Ok(BackgroundCancellation {
                record_id: record.id.clone(),
                agent_name: record.agent_name.clone(),
                exec_session_id: record.exec_session_id.clone(),
                already_terminal: true,
                stop_already_recorded,
            });
        }
        record.desired_enabled = false;
        record.status = BackgroundSubprocessStatus::Stopped;
        record.updated_at = Utc::now();
        record.ended_at = Some(Utc::now());
        Ok(BackgroundCancellation {
            record_id: record.id.clone(),
            agent_name: record.agent_name.clone(),
            exec_session_id: record.exec_session_id.clone(),
            already_terminal: false,
            stop_already_recorded: false,
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
