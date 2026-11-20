//! Graceful and forced cancellation of background subprocesses.

use super::*;

use super::controller_background_liveness::exec_session_is_missing;
use super::controller_background_query::resolve_background_target_mut;
use super::controller_background_settlement::{mark_background_ledger_write_pending, retry_attempt_available};

/// A stop whose exec session has not been closed yet.
///
/// A stop is one-way, and its durable outcome is not `Stopped` but "the
/// process is gone". Reporting `Stopped` before the close succeeds lets a
/// process outlive the stop: the close fault is dropped, and neither refresh
/// (which skips non-active records) nor restore (which selects only desired
/// records) ever retries it. The obligation is kept outside the record so it
/// survives a refresh that is cancelled or aborted between the close and its
/// settlement; only a successful close, or a spent retry allowance, clears it.
struct PendingStopClose {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    record_id: String,
    exec_session_id: String,
}

fn pending_stop_closes() -> &'static Mutex<Vec<PendingStopClose>> {
    static PENDING: std::sync::OnceLock<Mutex<Vec<PendingStopClose>>> = std::sync::OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(Vec::new()))
}

/// Applies `update` to the registry, pruning dropped trees first.
fn with_pending_stop_closes<R>(update: impl FnOnce(&mut Vec<PendingStopClose>) -> R) -> R {
    let mut registry = pending_stop_closes().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    registry.retain(|entry| entry.state.strong_count() > 0);
    update(&mut registry)
}

/// Records that `record_id` still owes the close of `exec_session_id`.
pub(super) fn mark_background_stop_close_pending(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) {
    with_pending_stop_closes(|registry| {
        let already_pending = registry.iter().any(|entry| {
            std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
                && entry.record_id == record_id
                && entry.exec_session_id == exec_session_id
        });
        if already_pending {
            return;
        }
        registry.push(PendingStopClose {
            state: Arc::downgrade(state),
            record_id: record_id.to_owned(),
            exec_session_id: exec_session_id.to_owned(),
        });
    });
}

/// Clears the close obligation for one launch after it succeeded or expired.
pub(super) fn clear_background_stop_close_pending(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) {
    with_pending_stop_closes(|registry| {
        registry.retain(|entry| {
            !(std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
                && entry.record_id == record_id
                && entry.exec_session_id == exec_session_id)
        });
    });
}

/// Reports whether this launch still owes the close of its exec session.
pub(super) fn background_stop_close_is_pending(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> bool {
    with_pending_stop_closes(|registry| {
        registry.iter().any(|entry| {
            std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
                && entry.record_id == record_id
                && entry.exec_session_id == exec_session_id
        })
    })
}

/// Returns every launch this controller still owes a close.
fn pending_background_stop_closes(state: &Arc<RwLock<ControllerState>>) -> Vec<(String, String)> {
    with_pending_stop_closes(|registry| {
        registry
            .iter()
            .filter(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
            .map(|entry| (entry.record_id.clone(), entry.exec_session_id.clone()))
            .collect()
    })
}

/// A scripted close outcome for the stop-close tests.
///
/// A live session manager has no natural way to fail or stall a close on
/// demand, and the obligation logic is only observable across that window. The
/// seam exists only for this crate's tests, so production closes are unchanged.
#[cfg(test)]
struct ScriptedClose {
    state: Weak<RwLock<ControllerState>>,
    exec_session_id: String,
    fault: Option<String>,
    entered: Option<Arc<Notify>>,
    release: Option<Arc<Notify>>,
}

#[cfg(test)]
fn scripted_closes() -> &'static Mutex<Vec<ScriptedClose>> {
    static SCRIPTED: std::sync::OnceLock<Mutex<Vec<ScriptedClose>>> = std::sync::OnceLock::new();
    SCRIPTED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Scripts the next close of `exec_session_id` to fail with `message`.
#[cfg(test)]
pub(super) fn fail_next_background_close(state: &Arc<RwLock<ControllerState>>, exec_session_id: &str, message: &str) {
    let mut scripted = scripted_closes().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    scripted.retain(|entry| entry.state.strong_count() > 0);
    scripted.push(ScriptedClose {
        state: Arc::downgrade(state),
        exec_session_id: exec_session_id.to_owned(),
        fault: Some(message.to_owned()),
        entered: None,
        release: None,
    });
}

/// Scripts the next close of `exec_session_id` to stall until released.
///
/// Returns the "close entered" and "release the close" signals, so a test can
/// abort the refresh while the close is in flight and then observe the
/// obligation it must not have lost.
#[cfg(test)]
pub(super) fn gate_next_background_close(
    state: &Arc<RwLock<ControllerState>>,
    exec_session_id: &str,
) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut scripted = scripted_closes().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    scripted.retain(|entry| entry.state.strong_count() > 0);
    scripted.push(ScriptedClose {
        state: Arc::downgrade(state),
        exec_session_id: exec_session_id.to_owned(),
        fault: None,
        entered: Some(Arc::clone(&entered)),
        release: Some(Arc::clone(&release)),
    });
    (entered, release)
}

#[cfg(test)]
fn take_scripted_close(state: &Arc<RwLock<ControllerState>>, exec_session_id: &str) -> Option<ScriptedClose> {
    let mut scripted = scripted_closes().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    scripted.retain(|entry| entry.state.strong_count() > 0);
    let index = scripted.iter().position(|entry| {
        std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)) && entry.exec_session_id == exec_session_id
    })?;
    Some(scripted.remove(index))
}

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
}

enum BackgroundCancellationMode {
    Graceful,
    Force,
}

/// The stable lead-in a stop's close fault carries in a record's error text.
const BACKGROUND_CLOSE_FAULT_PREFIX: &str = "Failed to close the background subprocess's exec session";

/// Records a stop's close fault without discarding a terminal outcome.
///
/// A record that already failed owns its `error` and `summary`: they describe
/// the run's outcome, which the stop's cleanup fault must not overwrite. The
/// fault is appended once so a retried failure stays visible without stacking
/// duplicate text. An active or merely stopped record has no outcome to keep,
/// so the fault becomes its error and the summary says the stop is pending.
fn record_background_close_fault(record: &mut BackgroundRecord, error: &anyhow::Error) {
    let fault = format!("{BACKGROUND_CLOSE_FAULT_PREFIX}: {error:#}");
    if record.status == BackgroundSubprocessStatus::Error {
        let already_recorded = record
            .error
            .as_deref()
            .is_some_and(|existing| existing.contains(BACKGROUND_CLOSE_FAULT_PREFIX));
        if !already_recorded {
            record.error = Some(match record.error.take() {
                Some(existing) => format!("{existing}; {fault}"),
                None => fault,
            });
        }
        return;
    }
    record.error = Some(fault);
    record.summary = Some("Background subagent is stopping".to_string());
}

impl SubagentController {
    /// Gracefully stops a background subprocess by setting its desired state to disabled.
    ///
    /// `owner` is the authority the caller presents; a session may stop only a
    /// record it owns, while the workspace operator may stop any record.
    pub async fn graceful_stop_background(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(owner, target, BackgroundCancellationMode::Graceful)
            .await
    }

    /// Force-cancels a background subprocess, closing its exec session immediately.
    ///
    /// `owner` is the authority the caller presents; a session may cancel only
    /// a record it owns, while the workspace operator may cancel any record.
    pub async fn force_cancel_background(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundSubprocessEntry> {
        self.cancel_background(owner, target, BackgroundCancellationMode::Force).await
    }

    async fn cancel_background(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
        mode: BackgroundCancellationMode,
    ) -> Result<BackgroundSubprocessEntry> {
        let cancellation = self.begin_background_cancellation(owner, target).await?;
        if cancellation.already_terminal {
            // A stopped or errored record is already settled. Preserve its
            // status, timestamps, and error text instead of rewriting it to
            // `Stopped` (which a transport would project as a successful
            // completion).
            //
            // A cancelled record's exec session may still be live even when its
            // status is already terminal: a graceful stop marks the record
            // `Stopped` before the process leaves, pruning only removes a
            // session that has already exited, and a stop another controller
            // persisted may never have reached the process. The close is the
            // stop's obligation, so it is attempted for either mode and its
            // failure is recorded rather than dropped.
            if !cancellation.exec_session_id.is_empty() {
                self.attempt_background_stop_close(&cancellation.record_id, &cancellation.exec_session_id)
                    .await;
            }
            // The cleared `desired_enabled` flag still has to reach disk so a
            // resumed session does not restore the cancelled record. The save
            // comes first: a first attempt can settle the record and then fail
            // here, leaving this retry as the only publisher of the terminal
            // state, so the snapshot is published only once it is committed.
            self.save_background_state().await?;
            return self.settle_background_status_and_publish(&cancellation.record_id).await;
        }
        self.log_background_cancellation(&cancellation, &mode);
        if cancellation.exec_session_id.is_empty() {
            // No session was recorded, so the stop has nothing left to close.
            self.settle_background_stop_closed(&cancellation.record_id, &cancellation.exec_session_id)
                .await;
        } else {
            if matches!(mode, BackgroundCancellationMode::Graceful) {
                // Signal first so a cooperative process can leave on its own.
                // The close below is still required: nothing guarantees the
                // signal was delivered or that the process honoured it.
                let _ = self.config.exec_sessions.terminate_session(&cancellation.exec_session_id).await;
            }
            self.attempt_background_stop_close(&cancellation.record_id, &cancellation.exec_session_id)
                .await;
        }
        self.finish_background_cancellation(&cancellation.record_id).await
    }

    /// Closes a background exec session, tolerating one that is already gone.
    ///
    /// A cancellation is idempotent: the session may have exited on its own or
    /// been closed by an earlier attempt. A typed not-found cause is therefore
    /// the expected outcome, not a failure to report. Any other fault is
    /// returned to the caller, which records it as an outstanding obligation
    /// rather than dropping it: a stop must not be reported complete while the
    /// process it names may still be live.
    pub(super) async fn close_background_exec_session(&self, exec_session_id: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(scripted) = take_scripted_close(&self.state, exec_session_id) {
            if let Some(entered) = scripted.entered {
                entered.notify_one();
            }
            if let Some(release) = scripted.release {
                release.notified().await;
            }
            if let Some(fault) = scripted.fault {
                return Err(anyhow!("{fault}"));
            }
        }
        match self.config.exec_sessions.close_session(exec_session_id).await {
            Ok(_) => Ok(()),
            Err(error) if exec_session_is_missing(&error) => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("Failed to close background exec session {exec_session_id}"))
            }
        }
    }

    /// Closes a stopped record's exec session and settles its durable outcome.
    ///
    /// The close is the stop's obligation, not best-effort cleanup: the
    /// obligation is recorded before the close is awaited, so an interrupted
    /// close is still retried; a success settles `Stopped` (or preserves a
    /// terminal `Error` the stop already recorded), while a fault leaves the
    /// record visibly pending. The single-attempt retry budget shared with
    /// restart settlement is spent here, so a second failure settles `Error`
    /// naming the close fault instead of retrying forever.
    pub(super) async fn attempt_background_stop_close(&self, record_id: &str, exec_session_id: &str) {
        if exec_session_id.is_empty() {
            self.settle_background_stop_closed(record_id, exec_session_id).await;
            return;
        }
        // The obligation is raised before the close is awaited, not after it
        // fails. An adopt or cancel that is interrupted inside the close has
        // already cleared `desired_enabled`, so neither refresh nor restore
        // would ever look at the record again; only an obligation recorded
        // beforehand can make the next reconcile finish the stop.
        mark_background_stop_close_pending(&self.state, record_id, exec_session_id);
        match self.close_background_exec_session(exec_session_id).await {
            Ok(()) => self.settle_background_stop_closed(record_id, exec_session_id).await,
            Err(error) => {
                self.settle_background_stop_close_failed(record_id, exec_session_id, &error)
                    .await
            }
        }
    }

    /// Applies a successful close to the record it stopped.
    async fn settle_background_stop_closed(&self, record_id: &str, exec_session_id: &str) {
        {
            let mut state = self.state.write().await;
            let Some(record) = state.background_children.get_mut(record_id) else {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return;
            };
            if record.exec_session_id != exec_session_id {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return;
            }
            if record.status.is_active() {
                record.status = BackgroundSubprocessStatus::Stopped;
                record.error = None;
                record.summary = Some("Background subagent stopped".to_string());
                record.ended_at.get_or_insert_with(Utc::now);
                record.updated_at = Utc::now();
            } else if record.status == BackgroundSubprocessStatus::Stopped {
                // A retried close may settle a stop whose record already
                // reached a terminal outcome. Preserve that outcome; only the
                // pending close's error text is cleared.
                record.error = None;
            }
            clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
        }
    }

    /// Keeps a failed close owed, or settles `Error` when its retry is spent.
    async fn settle_background_stop_close_failed(&self, record_id: &str, exec_session_id: &str, error: &anyhow::Error) {
        {
            let mut state = self.state.write().await;
            let Some(record) = state.background_children.get_mut(record_id) else {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return;
            };
            if record.exec_session_id != exec_session_id {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return;
            }
            if record.desired_enabled {
                // A replacement launch has taken the record since the stop
                // began, so this close no longer belongs to it.
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return;
            }
            record_background_close_fault(record, error);
            record.updated_at = Utc::now();
            if retry_attempt_available(record.restart_attempts) {
                record.restart_attempts = record.restart_attempts.saturating_add(1);
                // `Stopped` was written by an adopted stop before its close was
                // attempted. A live process is not stopped, so report the
                // pending state until the retry succeeds.
                if record.status == BackgroundSubprocessStatus::Stopped {
                    record.status = BackgroundSubprocessStatus::Running;
                }
                mark_background_stop_close_pending(&self.state, record_id, exec_session_id);
            } else {
                record.status = BackgroundSubprocessStatus::Error;
                record.ended_at.get_or_insert_with(Utc::now);
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
            }
        }
        tracing::warn!(
            record_id,
            exec_session_id,
            error = %error,
            "Failed to close a background subprocess's exec session; the stop remains owed"
        );
    }

    /// Retries every stop this controller still owes a close.
    ///
    /// Refresh and restore call this so an aborted or failed close is driven to
    /// a durable outcome. The obligation is cleared only after the close
    /// succeeds or its retry budget is spent, so an interrupted reconcile
    /// leaves it intact for the next one.
    pub(super) async fn retry_pending_background_stop_closes(&self) -> Result<()> {
        let pending = pending_background_stop_closes(&self.state);
        if pending.is_empty() {
            return Ok(());
        }
        let mut changed = Vec::new();
        for (record_id, exec_session_id) in pending {
            let still_owed = {
                let state = self.state.read().await;
                state
                    .background_children
                    .get(&record_id)
                    .is_some_and(|record| record.exec_session_id == exec_session_id && !record.desired_enabled)
            };
            if !still_owed {
                clear_background_stop_close_pending(&self.state, &record_id, &exec_session_id);
                continue;
            }
            self.attempt_background_stop_close(&record_id, &exec_session_id).await;
            changed.push(record_id);
        }
        if changed.is_empty() {
            return Ok(());
        }
        for record_id in changed {
            self.publish_background_status(&record_id).await;
        }
        if let Err(error) = self.save_background_state().await {
            // The stop is settled in memory but not yet durable. Remember the
            // owed write so refresh retries it before any reuse observes the
            // record.
            mark_background_ledger_write_pending(&self.state);
            tracing::warn!(
                error = %error,
                "Failed to persist a settled background stop"
            );
        }
        Ok(())
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

    async fn begin_background_cancellation(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
    ) -> Result<BackgroundCancellation> {
        let mut state = self.state.write().await;
        let record = resolve_background_target_mut(&mut state, owner, target)
            .ok_or_else(|| anyhow!("Unknown background subprocess {target}"))?;
        // The ownership check runs under the same write lock as the mutation,
        // so a record cannot change owners between the check and the stop.
        if !owner.background_owned(record.owner_session_id.as_deref()) {
            bail!("Unknown background subprocess {target}");
        }
        if !record.status.is_active() {
            // The record is already settled, so keep its status, timestamps,
            // and error text. The owner still asked to stop it, though, and
            // restore selection reads only `desired_enabled`, so clear the
            // flag to keep a settled record from being relaunched on resume.
            record.desired_enabled = false;
            record.updated_at = Utc::now();
            let record_id = record.id.clone();
            let agent_name = record.agent_name.clone();
            let exec_session_id = record.exec_session_id.clone();
            // A terminal record's session can still be live, so the stop still
            // owes its close. Raising the obligation here makes an interrupt
            // between this intent and the close retryable by the next reconcile.
            if !exec_session_id.is_empty() {
                mark_background_stop_close_pending(&self.state, record_id.as_str(), exec_session_id.as_str());
            }
            return Ok(BackgroundCancellation {
                record_id,
                agent_name,
                exec_session_id,
                already_terminal: true,
            });
        }
        // The stop is only complete once its exec session is closed, so the
        // record keeps its active status until then. Settling `Stopped` here
        // would report a process gone before anything tried to stop it.
        record.desired_enabled = false;
        record.summary = Some("Background subagent is stopping".to_string());
        record.updated_at = Utc::now();
        let record_id = record.id.clone();
        let agent_name = record.agent_name.clone();
        let exec_session_id = record.exec_session_id.clone();
        // The stop is owed from this moment, not from the first close attempt:
        // `desired_enabled` is already cleared, so an interrupt before the
        // close would otherwise leave a live process that neither refresh nor
        // restore ever looks at again.
        if !exec_session_id.is_empty() {
            mark_background_stop_close_pending(&self.state, record_id.as_str(), exec_session_id.as_str());
        }
        Ok(BackgroundCancellation {
            record_id,
            agent_name,
            exec_session_id,
            already_terminal: false,
        })
    }

    async fn finish_background_cancellation(&self, target: &str) -> Result<BackgroundSubprocessEntry> {
        self.refresh_background_archive_metadata(target).await?;
        self.save_background_state().await?;
        self.settle_background_status_and_publish(target).await
    }
}
