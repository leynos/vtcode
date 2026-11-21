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
/// settlement. A record-owned stop is cleared by a successful close or a spent
/// retry allowance; a close no record names is kept until it succeeds.
struct PendingStopClose {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    record_id: String,
    exec_session_id: String,
    /// Whether the close belongs to the record's current launch.
    ///
    /// A stop of the record is retried only while the record still names the
    /// session and no longer wants it. A declined launch's session is named by
    /// no record: the launch spawned it, then the record refused the commit,
    /// so the obligation must outlive the record moving to another launch or
    /// disappearing. Such a close is retried without consulting the record.
    record_owned: bool,
}

/// One close a reconcile still owes, with how it is retried.
struct PendingStopCloseSnapshot {
    record_id: String,
    exec_session_id: String,
    record_owned: bool,
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
    insert_pending_stop_close(state, record_id, exec_session_id, true);
}

/// Records that a declined launch owes the close of its own exec session.
///
/// The launch spawned a session before the record refused the commit, so no
/// record names it. Keying the obligation by the launch's own session lets a
/// reconcile retry it after the record has moved to another launch or gone.
pub(super) fn mark_detached_background_stop_close_pending(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) {
    insert_pending_stop_close(state, record_id, exec_session_id, false);
}

/// Adds one close obligation unless the same launch already owes it.
///
/// The first marker decides how the close is retried. A record-owned stop that
/// is already owed is never weakened to a detached one, and a detached close
/// stays retryable even if the same launch is later marked again: retrying it
/// without the record is always safe, because the session it names is the
/// launch's own.
fn insert_pending_stop_close(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
    record_owned: bool,
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
            record_owned,
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
fn pending_background_stop_closes(state: &Arc<RwLock<ControllerState>>) -> Vec<PendingStopCloseSnapshot> {
    with_pending_stop_closes(|registry| {
        registry
            .iter()
            .filter(|entry| std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state)))
            .map(|entry| PendingStopCloseSnapshot {
                record_id: entry.record_id.clone(),
                exec_session_id: entry.exec_session_id.clone(),
                record_owned: entry.record_owned,
            })
            .collect()
    })
}

/// The ordered close attempts made for one stop, and its last confirmed close.
///
/// A stop's close can be attempted more than once at a time: a cancellation,
/// an adopted stop and a reconcile retry all reach
/// [`SubagentController::attempt_background_stop_close`] independently. Each
/// attempt takes a token when it starts, in order. When an attempt confirms
/// the close, a slower attempt's later failure must not act on the record, so
/// a failure whose token predates the confirmed token is rejected. Without
/// that ordering the late failure rewound `Stopped` to `Running` and re-armed
/// an obligation the confirmed close had already discharged.
struct StopCloseAttempt {
    /// Keeps the key bound to a live state allocation so a dropped tree's entry
    /// can be pruned before its address is reused.
    state: Weak<RwLock<ControllerState>>,
    record_id: String,
    exec_session_id: String,
    /// The token the most recent attempt took.
    latest_token: u64,
    /// The token of the attempt whose close succeeded, if any.
    confirmed_token: Option<u64>,
}

fn stop_close_attempts() -> &'static Mutex<Vec<StopCloseAttempt>> {
    static ATTEMPTS: std::sync::OnceLock<Mutex<Vec<StopCloseAttempt>>> = std::sync::OnceLock::new();
    ATTEMPTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Applies `update` to the attempt registry, pruning dropped trees first.
fn with_stop_close_attempts<R>(update: impl FnOnce(&mut Vec<StopCloseAttempt>) -> R) -> R {
    let mut attempts = stop_close_attempts().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    attempts.retain(|entry| entry.state.strong_count() > 0);
    update(&mut attempts)
}

/// Reports whether `entry` belongs to the stop identified by the arguments.
fn stop_close_attempt_matches(
    entry: &StopCloseAttempt,
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> bool {
    std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
        && entry.record_id == record_id
        && entry.exec_session_id == exec_session_id
}

/// Starts one close attempt and returns the token that identifies it.
fn begin_background_stop_close_attempt(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> u64 {
    with_stop_close_attempts(|attempts| {
        if let Some(entry) = attempts
            .iter_mut()
            .find(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
        {
            entry.latest_token = entry.latest_token.saturating_add(1);
            return entry.latest_token;
        }
        attempts.push(StopCloseAttempt {
            state: Arc::downgrade(state),
            record_id: record_id.to_owned(),
            exec_session_id: exec_session_id.to_owned(),
            latest_token: 1,
            confirmed_token: None,
        });
        1
    })
}

/// Records that the attempt identified by `token` confirmed the close.
///
/// The caller holds the state write lock, so a slower failure for the same
/// stop either observes this before it settles the record or is rejected as
/// stale; it can never rewind the record past the confirmation.
fn confirm_background_stop_close_attempt(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
    token: u64,
) {
    with_stop_close_attempts(|attempts| {
        let Some(entry) = attempts
            .iter_mut()
            .find(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
        else {
            return;
        };
        entry.confirmed_token = Some(entry.confirmed_token.map_or(token, |confirmed| confirmed.max(token)));
    });
}

/// Reports whether a confirmed close overtook the attempt identified by `token`.
fn background_stop_close_failure_is_stale(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
    token: u64,
) -> bool {
    with_stop_close_attempts(|attempts| {
        attempts
            .iter()
            .find(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
            .and_then(|entry| entry.confirmed_token)
            .is_some_and(|confirmed| token < confirmed)
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
    script_gated_background_close(state, exec_session_id, None)
}

/// Scripts the next close to stall until released, then fail.
///
/// A failure that has to overlap another attempt needs both halves on one
/// scripted close: the stall holds the older attempt inside its close while a
/// newer one runs, and the fault makes it fail after that newer attempt has
/// already confirmed closure.
#[cfg(test)]
pub(super) fn fail_after_gated_background_close(
    state: &Arc<RwLock<ControllerState>>,
    exec_session_id: &str,
    message: &str,
) -> (Arc<Notify>, Arc<Notify>) {
    script_gated_background_close(state, exec_session_id, Some(message.to_owned()))
}

#[cfg(test)]
fn script_gated_background_close(
    state: &Arc<RwLock<ControllerState>>,
    exec_session_id: &str,
    fault: Option<String>,
) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut scripted = scripted_closes().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    scripted.retain(|entry| entry.state.strong_count() > 0);
    scripted.push(ScriptedClose {
        state: Arc::downgrade(state),
        exec_session_id: exec_session_id.to_owned(),
        fault,
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

/// The result of a stop request.
///
/// A stop is complete only once its exec session is closed, and the record
/// alone cannot express that: a close fault leaves the record pending (or
/// settles it `Error` once the retry allowance is spent) while the entry is
/// still a perfectly valid value. Reporting that entry as a successful stop
/// let an adapter announce a finished task whose process may still be live.
/// The outcome keeps the distinction so a caller can report the fault instead
/// of discarding it.
#[derive(Debug)]
pub enum BackgroundStopOutcome {
    /// The exec session is confirmed closed, or the record named none.
    Closed(BackgroundSubprocessEntry),
    /// The close could not be confirmed; the record's `error` names the fault.
    ClosePending(BackgroundSubprocessEntry),
}

impl BackgroundStopOutcome {
    /// Pairs a record with whether its close is confirmed.
    fn from_close(entry: BackgroundSubprocessEntry, close_confirmed: bool) -> Self {
        if close_confirmed {
            Self::Closed(entry)
        } else {
            Self::ClosePending(entry)
        }
    }

    /// Returns the record this stop settled or left pending.
    #[must_use]
    pub const fn entry(&self) -> &BackgroundSubprocessEntry {
        match self {
            Self::Closed(entry) | Self::ClosePending(entry) => entry,
        }
    }

    /// Consumes the outcome and returns the record it carries.
    #[must_use]
    pub fn into_entry(self) -> BackgroundSubprocessEntry {
        match self {
            Self::Closed(entry) | Self::ClosePending(entry) => entry,
        }
    }

    /// Reports whether the stop's exec session close is still unconfirmed.
    ///
    /// A caller that reports success to its client must inspect this: `true`
    /// means the process may still be live, and the record's `error` names the
    /// close fault.
    #[must_use]
    pub const fn is_close_pending(&self) -> bool {
        matches!(self, Self::ClosePending(_))
    }
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
        Ok(self
            .cancel_background(owner, target, BackgroundCancellationMode::Graceful)
            .await?
            .into_entry())
    }

    /// Force-cancels a background subprocess, closing its exec session immediately.
    ///
    /// `owner` is the authority the caller presents; a session may cancel only
    /// a record it owns, while the workspace operator may cancel any record.
    ///
    /// The outcome distinguishes a confirmed close from one that is still owed,
    /// so a caller cannot report a failed force close as a completed stop.
    pub async fn force_cancel_background(&self, owner: TaskOwner<'_>, target: &str) -> Result<BackgroundStopOutcome> {
        self.cancel_background(owner, target, BackgroundCancellationMode::Force).await
    }

    async fn cancel_background(
        &self,
        owner: TaskOwner<'_>,
        target: &str,
        mode: BackgroundCancellationMode,
    ) -> Result<BackgroundStopOutcome> {
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
            let close_confirmed = if cancellation.exec_session_id.is_empty() {
                true
            } else {
                self.attempt_background_stop_close(&cancellation.record_id, &cancellation.exec_session_id)
                    .await
            };
            // The cleared `desired_enabled` flag still has to reach disk so a
            // resumed session does not restore the cancelled record. The save
            // comes first: a first attempt can settle the record and then fail
            // here, leaving this retry as the only publisher of the terminal
            // state, so the snapshot is published only once it is committed.
            self.save_background_state().await?;
            let entry = self.settle_background_status_and_publish(&cancellation.record_id).await?;
            return Ok(BackgroundStopOutcome::from_close(entry, close_confirmed));
        }
        self.log_background_cancellation(&cancellation, &mode);
        let close_confirmed = if cancellation.exec_session_id.is_empty() {
            // No session was recorded, so the stop has nothing left to close.
            self.settle_background_stop_closed(&cancellation.record_id, &cancellation.exec_session_id, None)
                .await;
            true
        } else {
            if matches!(mode, BackgroundCancellationMode::Graceful) {
                // Signal first so a cooperative process can leave on its own.
                // The close below is still required: nothing guarantees the
                // signal was delivered or that the process honoured it.
                let _ = self.config.exec_sessions.terminate_session(&cancellation.exec_session_id).await;
            }
            self.attempt_background_stop_close(&cancellation.record_id, &cancellation.exec_session_id)
                .await
        };
        let entry = self.finish_background_cancellation(&cancellation.record_id).await?;
        Ok(BackgroundStopOutcome::from_close(entry, close_confirmed))
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
    ///
    /// Returns whether the close is confirmed. Overlapping attempts are
    /// ordered: an attempt that starts after another has confirmed the close
    /// and then succeeds itself reports confirmed closure, while a failure
    /// from an attempt the confirmed close overtook is discarded here instead
    /// of rewinding the record.
    pub(super) async fn attempt_background_stop_close(&self, record_id: &str, exec_session_id: &str) -> bool {
        if exec_session_id.is_empty() {
            // Nothing can be running under the record, so there is no close to
            // order against another attempt.
            self.settle_background_stop_closed(record_id, exec_session_id, None).await;
            return true;
        }
        // The obligation is raised before the close is awaited, not after it
        // fails. An adopt or cancel that is interrupted inside the close has
        // already cleared `desired_enabled`, so neither refresh nor restore
        // would ever look at the record again; only an obligation recorded
        // beforehand can make the next reconcile finish the stop.
        mark_background_stop_close_pending(&self.state, record_id, exec_session_id);
        let token = begin_background_stop_close_attempt(&self.state, record_id, exec_session_id);
        match self.close_background_exec_session(exec_session_id).await {
            Ok(()) => {
                self.settle_background_stop_closed(record_id, exec_session_id, Some(token))
                    .await;
                true
            }
            Err(error) => {
                self.settle_background_stop_close_failed(record_id, exec_session_id, token, &error)
                    .await
            }
        }
    }

    /// Applies a successful close to the record it stopped.
    ///
    /// `token` identifies the attempt whose close succeeded; `None` is the
    /// empty-session case, which has no competing attempt to order against.
    async fn settle_background_stop_closed(&self, record_id: &str, exec_session_id: &str, token: Option<u64>) {
        {
            let mut state = self.state.write().await;
            // The confirmation is recorded under the same lock that settles the
            // record, so an overlapping failure either sees it before it acts
            // or is rejected as stale.
            if let Some(token) = token {
                confirm_background_stop_close_attempt(&self.state, record_id, exec_session_id, token);
            }
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
    ///
    /// A failure is rejected outright when a confirmed close overtook its
    /// attempt. A slower attempt can fail after another one closed the same
    /// session, and acting on it would undo `Stopped`, re-arm an obligation the
    /// confirmed close discharged, and report a stopped task as running again.
    ///
    /// Returns whether the close is nevertheless confirmed: `true` when a
    /// newer attempt's confirmed close overtook this failure. A caller that
    /// answers a stop request reports that as a completed stop, because the
    /// process it names is gone even though this attempt did not close it.
    async fn settle_background_stop_close_failed(
        &self,
        record_id: &str,
        exec_session_id: &str,
        token: u64,
        error: &anyhow::Error,
    ) -> bool {
        let close_confirmed = {
            let mut state = self.state.write().await;
            let Some(record) = state.background_children.get_mut(record_id) else {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return false;
            };
            if record.exec_session_id != exec_session_id {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return false;
            }
            if record.desired_enabled {
                // A replacement launch has taken the record since the stop
                // began, so this close no longer belongs to it.
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                return false;
            }
            if background_stop_close_failure_is_stale(&self.state, record_id, exec_session_id, token) {
                // A newer attempt already confirmed the close. Leave the record
                // and the obligation that close settled untouched; a newer
                // attempt that has since re-armed the obligation owns it.
                true
            } else {
                record_background_close_fault(record, error);
                record.updated_at = Utc::now();
                if retry_attempt_available(record.restart_attempts) {
                    record.restart_attempts = record.restart_attempts.saturating_add(1);
                    // `Stopped` was written by an adopted stop before its close
                    // was attempted. A live process is not stopped, so report
                    // the pending state until the retry succeeds.
                    if record.status == BackgroundSubprocessStatus::Stopped {
                        record.status = BackgroundSubprocessStatus::Running;
                    }
                    mark_background_stop_close_pending(&self.state, record_id, exec_session_id);
                } else {
                    record.status = BackgroundSubprocessStatus::Error;
                    record.ended_at.get_or_insert_with(Utc::now);
                    clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                }
                false
            }
        };
        if close_confirmed {
            tracing::debug!(
                record_id,
                exec_session_id,
                "Ignored a close fault another attempt's confirmed close overtook"
            );
        } else {
            tracing::warn!(
                record_id,
                exec_session_id,
                error = %error,
                "Failed to close a background subprocess's exec session; the stop remains owed"
            );
        }
        close_confirmed
    }

    /// Closes a declined launch's exec session.
    ///
    /// The launch spawned this session and then found its record no longer
    /// wanted the commit, so no record names the session and no reconcile
    /// selects it. The obligation is recorded first and keyed by the launch's
    /// own session, so a close fault is retried by the next reconcile rather
    /// than reported as a warning and forgotten.
    pub(super) async fn close_declined_background_session(&self, record_id: &str, exec_session_id: &str) -> bool {
        mark_detached_background_stop_close_pending(&self.state, record_id, exec_session_id);
        match self.close_background_exec_session(exec_session_id).await {
            Ok(()) => {
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                true
            }
            Err(error) => {
                // No record carries this fault, so the warning is its only
                // report; the obligation keeps the retry alive.
                tracing::warn!(
                    record_id,
                    exec_session_id,
                    error = %error,
                    "Failed to close a declined background launch's exec session; the close remains owed"
                );
                false
            }
        }
    }

    /// Retries every stop this controller still owes a close.
    ///
    /// Refresh and restore call this so an aborted or failed close is driven to
    /// a durable outcome. The obligation is cleared only after the close
    /// succeeds or its retry budget is spent, so an interrupted reconcile
    /// leaves it intact for the next one. A close a declined launch owes is
    /// retried without consulting the record, because no record names that
    /// launch any more.
    pub(super) async fn retry_pending_background_stop_closes(&self) -> Result<()> {
        let pending = pending_background_stop_closes(&self.state);
        if pending.is_empty() {
            return Ok(());
        }
        let mut changed = Vec::new();
        for close in pending {
            let PendingStopCloseSnapshot { record_id, exec_session_id, record_owned } = close;
            if !record_owned {
                // No record names this launch's session, so nothing observable
                // changes and there is nothing to publish for it.
                self.close_declined_background_session(&record_id, &exec_session_id).await;
                continue;
            }
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
