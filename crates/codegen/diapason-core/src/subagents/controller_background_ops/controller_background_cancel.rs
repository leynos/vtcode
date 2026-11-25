//! Graceful and forced cancellation of background subprocesses.

use super::*;

use super::controller_background_liveness::exec_session_is_missing;
use super::controller_background_query::resolve_background_target_mut;
use super::controller_background_settlement::{
    clear_background_ledger_write_pending, mark_background_ledger_write_pending,
    pending_background_ledger_write_generation, retry_attempt_available,
};

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

/// Moves a pending close onto the canonical id a record was adopted under.
///
/// Adoption rekeys an ownerless record to its owner's id. The obligation was
/// registered under the id the launch had then, so leaving it there would make
/// `retry_pending_background_stop_closes` miss it and clear the obligation
/// without ever retrying the close. Only the same launch moves: an entry naming
/// a different exec session belongs to a replacement and stays with it.
pub(in crate::subagents) fn rekey_pending_background_stop_close(
    state: &Arc<RwLock<ControllerState>>,
    previous_record_id: &str,
    record_id: &str,
    exec_session_id: &str,
) {
    if previous_record_id == record_id {
        return;
    }
    with_pending_stop_closes(|registry| {
        for entry in registry.iter_mut() {
            if std::ptr::eq(entry.state.as_ptr(), Arc::as_ptr(state))
                && entry.record_id == previous_record_id
                && entry.exec_session_id == exec_session_id
            {
                entry.record_id = record_id.to_owned();
            }
        }
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
    /// The attempts that have started and not yet returned.
    ///
    /// The entry orders overlapping attempts, so it must outlive every attempt
    /// that can still consult a confirmed token. Once the last one returns,
    /// nothing can read the entry again and it is removed.
    in_flight: u32,
}

/// Keeps one close attempt counted until it returns.
///
/// Dropping the guard on every path — success, failure and cancellation — lets
/// the registry remove an entry once the last attempt that could still consult
/// its confirmed token has returned, instead of holding one entry per stopped
/// launch until the controller tree drops.
struct StopCloseAttemptToken {
    state: Weak<RwLock<ControllerState>>,
    record_id: String,
    exec_session_id: String,
    token: u64,
}

impl StopCloseAttemptToken {
    /// The token identifying this attempt in the registry's ordering.
    const fn token(&self) -> u64 {
        self.token
    }
}

impl Drop for StopCloseAttemptToken {
    fn drop(&mut self) {
        with_stop_close_attempts(|attempts| {
            let Some(index) = attempts.iter().position(|entry| {
                std::ptr::eq(entry.state.as_ptr(), self.state.as_ptr())
                    && entry.record_id == self.record_id
                    && entry.exec_session_id == self.exec_session_id
            }) else {
                return;
            };
            let remove = {
                let entry = &mut attempts[index];
                entry.in_flight = entry.in_flight.saturating_sub(1);
                entry.in_flight == 0 && entry.confirmed_token.is_some()
            };
            if remove {
                attempts.remove(index);
            }
        });
    }
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

/// Starts one close attempt and returns the guard keeping it counted.
///
/// The guard carries the attempt's token and removes the registry entry when
/// the last in-flight attempt returns after a confirmed close.
fn begin_background_stop_close_attempt(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> StopCloseAttemptToken {
    let token = with_stop_close_attempts(|attempts| {
        if let Some(entry) = attempts
            .iter_mut()
            .find(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
        {
            entry.latest_token = entry.latest_token.saturating_add(1);
            entry.in_flight = entry.in_flight.saturating_add(1);
            return entry.latest_token;
        }
        attempts.push(StopCloseAttempt {
            state: Arc::downgrade(state),
            record_id: record_id.to_owned(),
            exec_session_id: exec_session_id.to_owned(),
            latest_token: 1,
            confirmed_token: None,
            in_flight: 1,
        });
        1
    });
    StopCloseAttemptToken {
        state: Arc::downgrade(state),
        record_id: record_id.to_owned(),
        exec_session_id: exec_session_id.to_owned(),
        token,
    }
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

/// Reports whether any confirmed close for this stop makes a failure stale.
///
/// A confirmed close says the process is gone, which no later attempt can
/// contradict. Comparing tokens left a hole: a second attempt that started
/// after the first was confirmed takes a higher token, so its own failure was
/// not stale by that test and it rewound `Stopped` to `Running`, re-armed an
/// obligation the confirmed close had discharged, and reported a stopped task
/// as running again. The token ordering is therefore not consulted here — the
/// confirmation alone decides. An attempt that fails before any confirmation
/// still follows the ordinary retry path.
fn background_stop_close_failure_is_stale(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> bool {
    with_stop_close_attempts(|attempts| {
        attempts
            .iter()
            .find(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
            .is_some_and(|entry| entry.confirmed_token.is_some())
    })
}

/// Returns how many attempt entries are registered for one stop.
///
/// The registry is process-global, so a test asserting that an entry drained
/// must count only the stop it exercised.
#[cfg(test)]
pub(super) fn background_stop_close_attempt_count(
    state: &Arc<RwLock<ControllerState>>,
    record_id: &str,
    exec_session_id: &str,
) -> usize {
    with_stop_close_attempts(|attempts| {
        attempts
            .iter()
            .filter(|entry| stop_close_attempt_matches(entry, state, record_id, exec_session_id))
            .count()
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
pub(super) const BACKGROUND_CLOSE_FAULT_PREFIX: &str = "Failed to close the background subprocess's exec session";

/// The summary a record carries while a stop's close is still owed.
///
/// A listing shows it while the process may still be live; a confirmed close
/// replaces it with [`BACKGROUND_STOPPED_SUMMARY`]. The text is not a run
/// outcome, so clearing it must never discard a failure the run itself
/// reported.
const BACKGROUND_STOPPING_SUMMARY: &str = "Background subagent is stopping";

/// The summary a settled stop carries once its close is confirmed.
const BACKGROUND_STOPPED_SUMMARY: &str = "Background subagent stopped";

/// Reports whether a record's launch may still own a live process.
///
/// The question a start decision asks is "is the process this row names
/// confirmed gone?", not "does the row still carry an old fault?". Two durable
/// facts answer it, and neither is the status alone:
///
/// - A recorded close fault means the close was attempted and did not confirm
///   the process is gone. A retryable fault leaves the row active (the stop
///   rewinds `Stopped` to `Running`), so matching only `Error` would let a
///   second controller start a process beside the live one.
/// - An active row that no longer wants its launch has had a stop requested but
///   not confirmed: the stop's close is the only thing that settles it.
///
/// A later successful close removes the fault text and settles the row, which
/// is what frees it; the status and the fault are therefore read together.
pub(super) fn background_record_close_is_unconfirmed(record: &BackgroundRecord) -> bool {
    if record.exec_session_id.is_empty() {
        // Nothing can be running under a record that names no session.
        return false;
    }
    if record
        .error
        .as_deref()
        .is_some_and(|error| error.contains(BACKGROUND_CLOSE_FAULT_PREFIX))
    {
        return true;
    }
    !record.desired_enabled && record.status.is_active()
}

/// Removes the close fault a confirmed close discharges from an error.
///
/// The fault is appended as its own `; `-separated clause, so everything before
/// it is the outcome the run itself reported and is kept. A record whose error
/// was only the fault keeps no error, and an error that never carried the fault
/// is returned unchanged. Removing the clause is what records the confirmation:
/// the durable evidence of an unconfirmed close is the fault itself.
fn error_without_close_fault(error: Option<String>) -> Option<String> {
    let error = error?;
    let Some(index) = error.find(BACKGROUND_CLOSE_FAULT_PREFIX) else {
        return Some(error);
    };
    let retained = error[..index].trim_end().trim_end_matches(';').trim_end();
    (!retained.is_empty()).then(|| retained.to_owned())
}

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
    record.summary = Some(BACKGROUND_STOPPING_SUMMARY.to_string());
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
            let (close_confirmed, settled) = if cancellation.exec_session_id.is_empty() {
                (
                    true,
                    self.settle_background_stop_closed(&cancellation.record_id, &cancellation.exec_session_id, None)
                        .await,
                )
            } else {
                self.attempt_background_stop_close_settling(&cancellation.record_id, &cancellation.exec_session_id)
                    .await
            };
            // The cleared `desired_enabled` flag still has to reach disk so a
            // resumed session does not restore the cancelled record. The save
            // comes first: a first attempt can settle the record and then fail
            // here, leaving this retry as the only publisher of the terminal
            // state, so the snapshot is published only once it is committed.
            // A previous attempt may have settled this stop in memory and then
            // failed to write it. Its save is retried here, and the obligation
            // it observed is discharged only once that write succeeds; a
            // failure arriving during this save bumps the generation and
            // survives the clear.
            let owed_generation = pending_background_ledger_write_generation(&self.state);
            // The same window the ordinary stop exposes: the cleared intent and
            // the settled outcome exist only in memory from here until the save
            // commits, so a test parks here to prove the obligation covers it.
            #[cfg(test)]
            pass_background_cancellation_save_gate(&self.state).await;
            self.save_background_state().await?;
            if let Some(generation) = owed_generation {
                clear_background_ledger_write_pending(&self.state, generation);
            }
            let entry = self.publish_settled_background_stop(&cancellation.record_id, settled).await?;
            return Ok(BackgroundStopOutcome::from_close(entry, close_confirmed));
        }
        self.log_background_cancellation(&cancellation, &mode);
        let (close_confirmed, settled) = if cancellation.exec_session_id.is_empty() {
            // No session was recorded, so the stop has nothing left to close.
            (
                true,
                self.settle_background_stop_closed(&cancellation.record_id, &cancellation.exec_session_id, None)
                    .await,
            )
        } else {
            if matches!(mode, BackgroundCancellationMode::Graceful) {
                // Signal first so a cooperative process can leave on its own.
                // The close below is still required: nothing guarantees the
                // signal was delivered or that the process honoured it.
                let _ = self.config.exec_sessions.terminate_session(&cancellation.exec_session_id).await;
            }
            self.attempt_background_stop_close_settling(&cancellation.record_id, &cancellation.exec_session_id)
                .await
        };
        let entry = self.finish_background_cancellation(&cancellation.record_id, settled).await?;
        Ok(BackgroundStopOutcome::from_close(entry, close_confirmed))
    }

    /// Publishes a stop's outcome and answers with the launch it settled.
    ///
    /// The record id is stable across restarts, so re-resolving it after the
    /// close can find a replacement another caller has reserved and report that
    /// launch as the one this stop settled. The entry captured under the state
    /// lock that settled the matching exec session is the stop's own outcome;
    /// the id-based projection is only the fallback for a launch that had
    /// already moved on before it could be settled.
    async fn publish_settled_background_stop(
        &self,
        record_id: &str,
        settled: Option<BackgroundSubprocessEntry>,
    ) -> Result<BackgroundSubprocessEntry> {
        match settled {
            Some(entry) => {
                self.publish_background_status(record_id).await;
                Ok(entry)
            }
            None => self.settle_background_status_and_publish(record_id).await,
        }
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
        self.attempt_background_stop_close_settling(record_id, exec_session_id).await.0
    }

    /// Closes a stopped record's exec session and returns the launch it settled.
    ///
    /// The boolean says whether the close is confirmed; the entry is the record
    /// as it stood when the close settled the matching exec session, and is
    /// `None` when the record no longer named that launch. A caller answering a
    /// stop must report this entry: re-resolving the record id afterwards can
    /// find a replacement another caller has reserved and report it as stopped.
    async fn attempt_background_stop_close_settling(
        &self,
        record_id: &str,
        exec_session_id: &str,
    ) -> (bool, Option<BackgroundSubprocessEntry>) {
        if exec_session_id.is_empty() {
            // Nothing can be running under the record, so there is no close to
            // order against another attempt.
            return (true, self.settle_background_stop_closed(record_id, exec_session_id, None).await);
        }
        // The obligation is raised before the close is awaited, not after it
        // fails. An adopt or cancel that is interrupted inside the close has
        // already cleared `desired_enabled`, so neither refresh nor restore
        // would ever look at the record again; only an obligation recorded
        // beforehand can make the next reconcile finish the stop.
        mark_background_stop_close_pending(&self.state, record_id, exec_session_id);
        // The guard counts this attempt for its whole lifetime, including a
        // cancellation, so the registry entry survives exactly as long as an
        // attempt can still consult it.
        let attempt = begin_background_stop_close_attempt(&self.state, record_id, exec_session_id);
        match self.close_background_exec_session(exec_session_id).await {
            Ok(()) => (
                true,
                self.settle_background_stop_closed(record_id, exec_session_id, Some(attempt.token()))
                    .await,
            ),
            Err(error) => (
                self.settle_background_stop_close_failed(record_id, exec_session_id, &error)
                    .await,
                None,
            ),
        }
    }

    /// Applies a successful close to the record it stopped.
    ///
    /// `token` identifies the attempt whose close succeeded; `None` is the
    /// empty-session case, which has no competing attempt to order against.
    /// Returns the settled entry for the launch the close addressed, or `None`
    /// when the record no longer names that exec session.
    ///
    /// A settled record also raises the owed-write marker, in the same lock
    /// that clears its close obligation: from that moment the outcome exists
    /// only in memory, so an interrupted save must leave the write retryable.
    async fn settle_background_stop_closed(
        &self,
        record_id: &str,
        exec_session_id: &str,
        token: Option<u64>,
    ) -> Option<BackgroundSubprocessEntry> {
        let mut state = self.state.write().await;
        // The confirmation is recorded under the same lock that settles the
        // record, so an overlapping failure either sees it before it acts or is
        // rejected as stale.
        if let Some(token) = token {
            confirm_background_stop_close_attempt(&self.state, record_id, exec_session_id, token);
        }
        let Some(record) = state.background_children.get_mut(record_id) else {
            clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
            return None;
        };
        if record.exec_session_id != exec_session_id {
            clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
            return None;
        }
        if record.status.is_active() {
            record.status = BackgroundSubprocessStatus::Stopped;
            record.error = None;
            record.summary = Some(BACKGROUND_STOPPED_SUMMARY.to_string());
            record.ended_at.get_or_insert_with(Utc::now);
            record.updated_at = Utc::now();
        } else {
            // A retried or later close may settle a stop whose record already
            // reached a terminal outcome. Preserve that outcome: only the close
            // fault the confirmation discharges is removed, and a genuine run
            // failure keeps both its error and its summary. The confirmation
            // must reach the ledger as the newer copy, or the fault text would
            // keep blocking a later start.
            record.error = error_without_close_fault(record.error.take());
            record.updated_at = Utc::now();
            // The pending-stop summary is cleanup state, not a run outcome. A
            // close that exhausted its retry allowance leaves it on an `Error`
            // record, so a later confirmed close must replace it even though
            // the status and any run error stay exactly as they were.
            if record.summary.as_deref() == Some(BACKGROUND_STOPPING_SUMMARY) {
                record.summary = Some(BACKGROUND_STOPPED_SUMMARY.to_string());
            }
        }
        // The settled outcome exists only in memory until the caller saves it.
        // The owed write is recorded in the same settlement that clears the
        // close obligation, so a caller dropped before its save leaves the
        // write retryable instead of losing the stop.
        mark_background_ledger_write_pending(&self.state);
        clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
        // Captured under the same lock that applied the close, while the record
        // still names the launch this stop addressed.
        Some(record.build_status_entry())
    }

    /// Keeps a failed close owed, or settles `Error` when its retry is spent.
    ///
    /// A failure is rejected outright when a confirmed close overtook its
    /// attempt. A slower attempt can fail after another one closed the same
    /// session, and acting on it would undo `Stopped`, re-arm an obligation the
    /// confirmed close discharged, and report a stopped task as running again.
    ///
    /// Returns whether the close is nevertheless confirmed: `true` when any
    /// attempt's confirmed close overtook this failure. A caller that answers a
    /// stop request reports that as a completed stop, because the process it
    /// names is gone even though this attempt did not close it.
    async fn settle_background_stop_close_failed(
        &self,
        record_id: &str,
        exec_session_id: &str,
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
            if background_stop_close_failure_is_stale(&self.state, record_id, exec_session_id) {
                // The close is already confirmed, by this attempt's successor or
                // by any attempt overlapping it. Leave the record untouched, and
                // clear the obligation this attempt raised when it started: the
                // attempt that started after the confirmation re-armed a stop
                // the confirmed close had already discharged, and leaving that
                // marker would report a stopped task as still owing its close.
                clear_background_stop_close_pending(&self.state, record_id, exec_session_id);
                true
            } else {
                record_background_close_fault(record, error);
                record.updated_at = Utc::now();
                if retry_attempt_available(record.restart_attempts) {
                    record.restart_attempts = record.restart_attempts.saturating_add(1);
                    // `Stopped` was written by an adopted stop before its close
                    // was attempted. A live process is not stopped, so report
                    // the pending state until the retry succeeds. The end
                    // instant belongs to the settled state and must go with it:
                    // the projection maps `ended_at` without checking status,
                    // and the ACP contract reserves an end instant for terminal
                    // tasks.
                    if record.status == BackgroundSubprocessStatus::Stopped {
                        record.status = BackgroundSubprocessStatus::Running;
                        record.ended_at = None;
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
    ///
    /// The settlement that discharges a close obligation also records the
    /// ledger write it owes. This save clears that generation only once it has
    /// succeeded, so a caller dropped between the two leaves the settled stop
    /// retryable by the next reconcile instead of losing it.
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
        // The settlements above raised the owed-write obligation before they
        // cleared each close obligation. Capture the generation they created so
        // this save discharges exactly it, and only if the save succeeds.
        let owed_generation = pending_background_ledger_write_generation(&self.state);
        #[cfg(test)]
        pass_background_stop_retry_save_gate(&self.state).await;
        if let Err(error) = self.save_background_state().await {
            // The stop is settled in memory but not yet durable. Keep the
            // obligation raised (and bump it, in case this pass settled nothing)
            // so refresh retries the write before any reuse observes the record.
            mark_background_ledger_write_pending(&self.state);
            tracing::warn!(
                error = %error,
                "Failed to persist a settled background stop"
            );
        } else if let Some(generation) = owed_generation {
            clear_background_ledger_write_pending(&self.state, generation);
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
            let intent_changed = record.desired_enabled;
            record.desired_enabled = false;
            record.updated_at = Utc::now();
            let record_id = record.id.clone();
            let agent_name = record.agent_name.clone();
            let exec_session_id = record.exec_session_id.clone();
            // The cleared intent exists only in memory until a save reaches the
            // ledger, and restore selects on it: a caller dropped before that
            // save would relaunch the cancelled task on the next resume. The
            // obligation is raised under this lock, so the window opens only
            // once the intent is already recorded as owed.
            if intent_changed {
                mark_background_ledger_write_pending(&self.state);
            }
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
        record.summary = Some(BACKGROUND_STOPPING_SUMMARY.to_string());
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

    async fn finish_background_cancellation(
        &self,
        target: &str,
        settled: Option<BackgroundSubprocessEntry>,
    ) -> Result<BackgroundSubprocessEntry> {
        // The close has already discharged the stop's obligation, so between it
        // and this save the settled record exists only in memory. The archive
        // refresh is awaited in that window; an interrupted refresh would
        // otherwise lose the save, and nothing would publish the stop again.
        // The close's settlement raised the owed-write marker already; reuse
        // that generation so the successful save discharges it, and raise one
        // here only if the close settled nothing.
        let generation = pending_background_ledger_write_generation(&self.state)
            .unwrap_or_else(|| mark_background_ledger_write_pending(&self.state));
        #[cfg(test)]
        pass_background_cancellation_save_gate(&self.state).await;
        // The archive refresh only attaches projection paths; it does not
        // decide the stop's outcome. Returning its fault here would skip the
        // save and the publication below, losing a stop the close already
        // confirmed and leaving the process's record unsaved for as long as the
        // archive stays unreadable. Refresh treats the same fault the same way:
        // report it against the record and carry on.
        if let Err(error) = self.refresh_background_archive_metadata(target).await {
            tracing::warn!(
                record_id = target,
                error = %error,
                "Could not refresh the background archive metadata; leaving the stop as settled"
            );
        }
        self.save_background_state().await?;
        clear_background_ledger_write_pending(&self.state, generation);
        self.publish_settled_background_stop(target, settled).await
    }
}

/// A test gate before a stop's archive refresh and save.
///
/// The close has already settled the record and raised the owed-write marker by
/// the time the archive refresh is awaited. Parking here lets a test drop the
/// caller in exactly that window and prove the stop is still retried.
#[cfg(test)]
struct BackgroundCancellationSaveGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
fn background_cancellation_save_gates() -> &'static Mutex<Vec<BackgroundCancellationSaveGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<BackgroundCancellationSaveGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next cancellation save of `state`.
#[cfg(test)]
pub(super) fn gate_next_background_cancellation_save(
    state: &Arc<RwLock<ControllerState>>,
) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = background_cancellation_save_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(BackgroundCancellationSaveGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

#[cfg(test)]
async fn pass_background_cancellation_save_gate(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = background_cancellation_save_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| gate.state.strong_count() > 0);
        let index = gates
            .iter()
            .position(|gate| std::ptr::eq(gate.state.as_ptr(), Arc::as_ptr(state)));
        index.map(|index| gates.remove(index))
    };
    let Some(gate) = gate else {
        return;
    };
    gate.entered.notify_one();
    gate.release.notified().await;
}

/// A test gate before a reconciled stop's ledger save.
///
/// The close has already settled the record and cleared its close obligation by
/// the time this gate is awaited, so parking here lets a test drop the caller
/// in exactly the window where only the owed-write marker can preserve the
/// stop.
#[cfg(test)]
struct BackgroundStopRetrySaveGate {
    state: Weak<RwLock<ControllerState>>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

#[cfg(test)]
fn background_stop_retry_save_gates() -> &'static Mutex<Vec<BackgroundStopRetrySaveGate>> {
    static GATES: std::sync::OnceLock<Mutex<Vec<BackgroundStopRetrySaveGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Arms a gate that holds the next reconciled-stop save of `state`.
#[cfg(test)]
fn gate_next_background_stop_retry_save(state: &Arc<RwLock<ControllerState>>) -> (Arc<Notify>, Arc<Notify>) {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let mut gates = background_stop_retry_save_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.state.strong_count() > 0);
    gates.push(BackgroundStopRetrySaveGate {
        state: Arc::downgrade(state),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

#[cfg(test)]
async fn pass_background_stop_retry_save_gate(state: &Arc<RwLock<ControllerState>>) {
    let gate = {
        let mut gates = background_stop_retry_save_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| gate.state.strong_count() > 0);
        let index = gates
            .iter()
            .position(|gate| std::ptr::eq(gate.state.as_ptr(), Arc::as_ptr(state)));
        index.map(|index| gates.remove(index))
    };
    let Some(gate) = gate else {
        return;
    };
    gate.entered.notify_one();
    gate.release.notified().await;
}

#[cfg(test)]
mod stop_close_settlement_tests {
    //! Pins the settlement obligations a confirmed close leaves behind.
    //!
    //! The attempt registry must drain itself, a reconciled stop must stay
    //! owed to the ledger across an interrupted save, and a later close must
    //! not leave the pending-stop summary claiming the stop is in progress.

    use std::path::PathBuf;

    use tempfile::TempDir;

    use crate::config::DiapasonConfig;
    use crate::config::types::ReasoningEffortLevel;
    use crate::subagents::background::load_background_state;
    use crate::tools::exec_session::ExecSessionManager;
    use crate::tools::registry::PtySessionManager;

    use super::*;

    const WORKER: &str = "background-worker";
    const EXEC: &str = "exec-attempt";

    fn test_config(workspace_root: PathBuf) -> SubagentControllerConfig {
        let vt_cfg = DiapasonConfig::default();
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
        let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        SubagentControllerConfig {
            workspace_root,
            parent_session_id: "parent-session".to_string(),
            parent_model: "gpt-5.6-sol".to_string(),
            parent_provider: "openai".to_string(),
            parent_reasoning_effort: ReasoningEffortLevel::Medium,
            api_key: "test-key".to_string(),
            vt_cfg,
            openai_chatgpt_auth: None,
            depth: 0,
            workspace_gated: false,
            exec_sessions,
            output_reader: None,
            pty_manager: pty_sessions.manager().clone(),
            managed_background_runtime: false,
            lock_scope: None,
        }
    }

    fn background_record() -> BackgroundRecord {
        BackgroundRecord {
            id: WORKER.to_string(),
            owner_session_id: None,
            agent_name: "worker".to_string(),
            display_label: "worker".to_string(),
            description: "worker".to_string(),
            source: "builtin".to_string(),
            colour: None,
            session_id: format!("session-{WORKER}"),
            parent_session_id: "parent-session".to_string(),
            exec_session_id: EXEC.to_string(),
            desired_enabled: false,
            status: BackgroundSubprocessStatus::Running,
            created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
            updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
            started_at: None,
            ended_at: None,
            pid: None,
            prompt: "work".to_string(),
            summary: None,
            error: None,
            archive_path: None,
            transcript_path: None,
            max_turns: None,
            model_override: None,
            reasoning_override: None,
            restart_attempts: 0,
        }
    }

    /// An entry survives only while an attempt can still consult it.
    ///
    /// The registry orders overlapping attempts by token, so a confirmed token
    /// must outlive every attempt that began before the confirmation. Once the
    /// last attempt returns, nothing can read the entry again; keeping it until
    /// the controller tree drops leaks one entry per stopped launch.
    #[tokio::test]
    async fn a_confirmed_stop_close_drains_its_attempt_registry_entry() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), background_record());
        }

        fail_next_background_close(&controller.state, EXEC, "first close fault");
        assert!(!controller.attempt_background_stop_close(WORKER, EXEC).await, "the scripted close fails");
        assert_eq!(
            background_stop_close_attempt_count(&controller.state, WORKER, EXEC),
            1,
            "an unconfirmed attempt must stay registered so a slower failure can be ordered against it"
        );

        assert!(controller.attempt_background_stop_close(WORKER, EXEC).await, "the missing session counts as closed");
        assert_eq!(
            background_stop_close_attempt_count(&controller.state, WORKER, EXEC),
            0,
            "the registry must drain once the last attempt returns after a confirmed close"
        );
        assert!(!background_stop_close_is_pending(&controller.state, WORKER, EXEC));

        controller.signal_shutdown().await;
    }

    /// A reconcile dropped between the close and its save keeps the write owed.
    ///
    /// The close settles `Stopped` in memory and clears the close obligation,
    /// so nothing would retry it if the caller disappeared before the save. The
    /// owed-write obligation is recorded in the same settlement, which leaves
    /// the stopped record retryable by the next refresh; only a successful save
    /// may discharge it.
    #[tokio::test]
    async fn a_reconciled_stop_dropped_before_its_save_stays_owed() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), background_record());
        }
        mark_background_stop_close_pending(&controller.state, WORKER, EXEC);

        let (entered, _release) = gate_next_background_stop_retry_save(&controller.state);
        {
            let retry = controller.retry_pending_background_stop_closes();
            tokio::pin!(retry);
            tokio::select! {
                result = &mut retry => panic!("the gated reconcile saved before it was dropped: {result:?}"),
                () = entered.notified() => {}
            }
        }

        {
            let state = controller.state.read().await;
            let record = state.background_children.get(WORKER).expect("record");
            assert_eq!(
                record.status,
                BackgroundSubprocessStatus::Stopped,
                "the confirmed close settles the stop before the interrupted save"
            );
        }
        assert!(
            !background_stop_close_is_pending(&controller.state, WORKER, EXEC),
            "the confirmed close discharges its close obligation"
        );
        assert!(
            pending_background_ledger_write_generation(&controller.state).is_some(),
            "the settlement must record the write it owes before clearing the close obligation"
        );

        assert!(
            controller
                .retry_owed_background_ledger_write()
                .await
                .expect("the retried write succeeds"),
            "the interrupted save must still be owed to the next reconcile"
        );
        assert!(
            pending_background_ledger_write_generation(&controller.state).is_none(),
            "a successful write discharges the obligation"
        );
        let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
        let stored = persisted
            .records
            .iter()
            .find(|record| record.id == WORKER)
            .expect("the retried write reaches the ledger");
        assert_eq!(stored.status, BackgroundSubprocessStatus::Stopped);
        assert!(!stored.desired_enabled, "the written stop clears the desired flag");

        controller.signal_shutdown().await;
    }

    /// A later confirmed close replaces the pending-stop summary.
    ///
    /// A close fault that exhausts its retry allowance leaves an `Error` record
    /// with the summary "Background subagent is stopping". A later force cancel
    /// can confirm closure, and the entry must no longer claim the stop is in
    /// progress, while a genuine run failure keeps its own summary and error.
    #[tokio::test]
    async fn a_later_confirmed_close_clears_the_pending_stop_summary() {
        let temp = TempDir::new().expect("tempdir");
        let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
            .await
            .expect("controller");
        let mut record = background_record();
        record.status = BackgroundSubprocessStatus::Stopped;
        record.error = Some(format!("{BACKGROUND_CLOSE_FAULT_PREFIX}: close transport fault"));
        record.summary = Some(BACKGROUND_STOPPING_SUMMARY.to_string());
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(WORKER.to_string(), record);
        }
        mark_background_stop_close_pending(&controller.state, WORKER, EXEC);

        controller
            .retry_pending_background_stop_closes()
            .await
            .expect("the missing session counts as closed");

        let state = controller.state.read().await;
        let record = state.background_children.get(WORKER).expect("record");
        assert_eq!(record.status, BackgroundSubprocessStatus::Stopped, "the terminal outcome is preserved");
        assert!(record.error.is_none(), "the pending close's own error text is cleared");
        assert_eq!(
            record.summary.as_deref(),
            Some(BACKGROUND_STOPPED_SUMMARY),
            "a confirmed close must not leave the pending-stop summary in place"
        );
        drop(state);

        controller.signal_shutdown().await;
    }
}
