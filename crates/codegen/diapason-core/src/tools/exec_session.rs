//! Manages persistent command and PTY process sessions used by execution tools.
//!
//! The controller background launch-id convention lives with the background
//! output adapter that authorizes archives by it; it is re-exported here
//! because the exec-session manager and the launch producer name it through
//! this module.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use diapason_bash_runner::{PipeSpawnOptions, ProcessHandle, spawn_pipe_process_with_options};
use hashbrown::HashMap;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex, RwLock, watch};
use tokio::task::JoinHandle;

use crate::sandboxing::filter_sensitive_env;
use crate::tools::ExecSessionId;
use crate::tools::pty::PtySize;
use crate::tools::registry::{PtySessionGuard, PtySessionManager};
use crate::tools::types::DiapasonExecSession;
use crate::utils::path::{canonicalize_workspace, ensure_path_within_workspace};
use crate::zsh_exec_bridge::ZshExecBridgeSession;

const PIPE_OUTPUT_HEAD_BYTES: usize = 8 * 1024;

const PIPE_OUTPUT_TAIL_BYTES: usize = 8 * 1024;

/// The controller launch-id convention, re-exported for this module's callers.
pub(crate) use crate::subagents::controller_exec_session_task_id;

/// A continuation referenced an exec session that is no longer active.
#[derive(Debug, thiserror::Error)]
#[error("exec session '{session_id}' not found")]
pub(crate) struct ExecSessionNotFound {
    pub(crate) session_id: String,
}

impl ExecSessionNotFound {
    /// Build the typed error for a continuation naming an unknown session.
    fn new(session_id: &str) -> Self {
        Self { session_id: session_id.to_string() }
    }
}

/// Reports whether a read of an exec session failed because the session is
/// genuinely gone.
///
/// Only a typed not-found cause counts, from either storage layer. A close
/// terminates and releases the backend record before it removes the dispatch
/// record, so a read that races the close can find the dispatch record and
/// miss the backend handle; that is the same expected end as a read that misses
/// the record, and treating it as a fault would report a fault for a session
/// that has simply exited. Any other failure — a backend inconsistency or an
/// I/O fault — told the caller nothing about the process, so it must stay
/// distinguishable from an exit.
///
/// This lives next to the typed cause so the liveness projection and the
/// background-output adapter cannot drift apart on what counts as an exit.
pub(crate) fn is_missing_exec_session(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.is::<ExecSessionNotFound>() || cause.is::<crate::tools::pty::PtySessionNotFound>())
}

/// A `write_stdin` continuation lost its session after the input bytes were
/// already handed to the session, so the effect of that input is uncertain.
#[derive(Debug, thiserror::Error)]
#[error("exec session '{session_id}' ended after the write was delivered")]
pub(crate) struct ExecSessionLostAfterInput {
    pub(crate) session_id: String,
    /// The underlying channel error, retained for diagnosis.
    ///
    /// The typed cause above, not this message, is what callers classify and
    /// what the client sees, so retaining the original error does not widen the
    /// client-visible detail.
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl ExecSessionLostAfterInput {
    /// Build the typed error for a write that failed after the first send.
    ///
    /// Production always has a channel failure to retain, so this
    /// source-less form exists only for tests that exercise classification
    /// without constructing a live session.
    #[cfg(test)]
    pub(crate) fn new(session_id: &str) -> Self {
        Self { session_id: session_id.to_string(), source: None }
    }

    /// Build the typed error and retain the underlying failure for diagnosis.
    pub(crate) fn with_source(session_id: &str, source: anyhow::Error) -> Self {
        Self {
            session_id: session_id.to_string(),
            source: Some(source.into()),
        }
    }
}

/// A pipe-backed exec session whose stdin writer was already closed before any
/// input could be delivered.
///
/// This is distinct from [`ExecSessionNotFound`]: the session record still
/// exists, but its process has exited or its writer task has stopped, so the
/// write cannot reach the child. Because nothing was delivered, the failure
/// carries no partial-state risk.
#[derive(Debug, thiserror::Error)]
#[error("exec session '{session_id}' is no longer writable")]
pub(crate) struct ExecSessionWriterClosed {
    pub(crate) session_id: String,
    /// The underlying channel error, retained for diagnosis.
    ///
    /// The typed cause above, not this message, is what callers classify and
    /// what the client sees, so retaining the original error does not widen the
    /// client-visible detail.
    #[source]
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

/// Wrap [`ExecSessionNotFound`] so callers can recover the typed source.
fn missing_session(session_id: &str) -> anyhow::Error {
    anyhow::Error::new(ExecSessionNotFound::new(session_id))
}

/// Map a failed stdin write to the typed cause that reflects whether any input
/// had already been delivered.
///
/// A failure before the first write means nothing reached the child, so it is
/// [`ExecSessionWriterClosed`]. A failure after the first write means the input
/// was already handed to the session, so it is [`ExecSessionLostAfterInput`]
/// and callers must treat partial state as possible.
///
/// `source` is the underlying send failure when one exists. It is kept as the
/// typed error's source so the original cause is not discarded, while the
/// classification and the client-visible message continue to come from the
/// typed cause alone.
fn write_failure(
    session_id: &str,
    input_delivered: bool,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
) -> anyhow::Error {
    if input_delivered {
        anyhow::Error::new(ExecSessionLostAfterInput { session_id: session_id.to_string(), source })
    } else {
        anyhow::Error::new(ExecSessionWriterClosed { session_id: session_id.to_string(), source })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PipeOutputStats {
    pub total_bytes: u64,
    pub truncated: bool,
    pub spool_path: String,
    pub spool_available: bool,
    pub spool_complete: bool,
}

#[derive(Default)]
struct PipeOutputBuffer {
    pending: Mutex<PipeOutputWindow>,
    total_bytes: AtomicU64,
    truncated: AtomicBool,
}

#[derive(Default)]
struct PipeOutputWindow {
    head: String,
    tail: String,
    total_bytes: u64,
    truncated: bool,
}

impl PipeOutputBuffer {
    async fn append(&self, chunk: &str, raw_byte_count: usize) {
        let mut pending = self.pending.lock().await;
        pending.total_bytes = pending.total_bytes.saturating_add(raw_byte_count as u64);
        self.total_bytes.fetch_add(raw_byte_count as u64, Ordering::Relaxed);
        if pending.head.len() < PIPE_OUTPUT_HEAD_BYTES {
            let remaining = PIPE_OUTPUT_HEAD_BYTES - pending.head.len();
            let end = chunk.floor_char_boundary(remaining.min(chunk.len()));
            pending.head.push_str(&chunk[..end]);
            if end < chunk.len() {
                pending.truncated = true;
                self.truncated.store(true, Ordering::Relaxed);
            }
        }

        // Operate on `pending.tail` in place. The previous code cloned the tail,
        // mutated the clone, then assigned it back — an O(tail) copy on every
        // output chunk. Since `pending` is already a &mut MutexGuard, in-place
        // mutation is equivalent and avoids the per-chunk allocation.
        pending.tail.push_str(chunk);
        if pending.tail.len() > PIPE_OUTPUT_TAIL_BYTES {
            let start = pending.tail.ceil_char_boundary(pending.tail.len() - PIPE_OUTPUT_TAIL_BYTES);
            pending.tail.drain(..start);
            pending.truncated = true;
            self.truncated.store(true, Ordering::Relaxed);
        }
    }

    async fn peek_pending(&self) -> Option<String> {
        let pending = self.pending.lock().await;
        if pending.total_bytes == 0 {
            None
        } else {
            Some(pending.preview())
        }
    }

    async fn drain_pending(&self) -> Option<String> {
        let mut pending = self.pending.lock().await;
        if pending.total_bytes == 0 {
            None
        } else {
            Some(std::mem::take(&mut *pending).preview())
        }
    }

    async fn stats(&self) -> (u64, bool) {
        (self.total_bytes.load(Ordering::Relaxed), self.truncated.load(Ordering::Relaxed))
    }
}

impl PipeOutputWindow {
    fn preview(&self) -> String {
        if !self.truncated {
            return self.head.clone();
        }
        if self.head == self.tail {
            return self.head.clone();
        }
        format!("{}\n[output preview truncated]\n{}", self.head, self.tail)
    }
}

struct PipeSessionRecord {
    metadata: DiapasonExecSession,
    handle: Arc<ProcessHandle>,
    output: Arc<PipeOutputBuffer>,
    output_task: Mutex<Option<JoinHandle<()>>>,
    exit_task: Mutex<Option<JoinHandle<()>>>,
    activity_tx: watch::Sender<u64>,
    spool: PipeSpoolState,
}

struct PipeSpoolState {
    path: String,
    ready: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
}

impl PipeSessionRecord {
    fn new(
        metadata: DiapasonExecSession,
        handle: Arc<ProcessHandle>,
        output: Arc<PipeOutputBuffer>,
        output_task: JoinHandle<()>,
        exit_task: JoinHandle<()>,
        activity_tx: watch::Sender<u64>,
        spool: PipeSpoolState,
    ) -> Self {
        Self {
            metadata,
            handle,
            output,
            output_task: Mutex::new(Some(output_task)),
            exit_task: Mutex::new(Some(exit_task)),
            activity_tx,
            spool,
        }
    }
}

#[derive(Clone)]
struct PipeSessionManager {
    workspace_root: PathBuf,
    sessions: Arc<RwLock<HashMap<ExecSessionId, Arc<PipeSessionRecord>>>>,
    /// A scripted fault for the close-path termination.
    ///
    /// The record-retention rule for a failed kill is only observable when the
    /// backend cannot signal the child, which a live manager cannot produce on
    /// demand. The seam is `#[cfg(test)]`-only, so a production build neither
    /// compiles the field nor the branch that reads it.
    #[cfg(test)]
    termination_failure: Arc<std::sync::Mutex<Option<String>>>,
}

impl PipeSessionManager {
    fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root: canonicalize_workspace(&workspace_root),
            sessions: Arc::new(RwLock::new(HashMap::new())),
            #[cfg(test)]
            termination_failure: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Spawns one pipe session and returns it with its backend record.
    ///
    /// The record is returned so a create that is cancelled before it reaches
    /// the dispatch map can close the process it spawned; see
    /// [`ExecSessionManager::create_pipe_session_with_sandbox`].
    async fn create_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
    ) -> Result<(DiapasonExecSession, Arc<PipeSessionRecord>)> {
        if command.is_empty() {
            return Err(anyhow!("exec session command cannot be empty"));
        }
        let working_dir = canonicalize_workspace(&working_dir);
        self.ensure_within_workspace(&working_dir)?;

        // Hold the write lock across check → spawn → insert so two concurrent
        // creates with the same session_id cannot both pass the existence check
        // and spawn; the second insert would silently overwrite the first and
        // leak its spawned process and background tasks.
        let mut sessions = self.sessions.write().await;
        if sessions.contains_key(session_id.as_str()) {
            return Err(anyhow!("exec session '{}' already exists", session_id.as_str()));
        }

        let mut command_parts = command;
        let program = command_parts.remove(0);
        let args = command_parts;

        let opts = PipeSpawnOptions::new(program.clone(), working_dir.clone())
            .args(args.clone())
            .env(env)
            .lossless_output(true);
        let spawned = spawn_pipe_process_with_options(opts)
            .await
            .with_context(|| format!("failed to spawn pipe session '{session_id}'"))?;

        let metadata = DiapasonExecSession {
            id: session_id.clone(),
            backend: "pipe".to_string(),
            command: program,
            args,
            working_dir: Some(self.format_working_dir(&working_dir)),
            rows: None,
            cols: None,
            child_pid: None,
            started_at: Some(Utc::now()),
            lifecycle_state: Some(crate::tools::types::DiapasonSessionLifecycleState::Running),
            exit_code: None,
        };

        let handle = Arc::new(spawned.session);
        let output = Arc::new(PipeOutputBuffer::default());
        let output_clone = Arc::clone(&output);
        let mut output_rx = spawned.reliable_output_rx;
        let output_handle = Arc::clone(&handle);
        let (activity_tx, _) = watch::channel(0u64);
        let output_activity_tx = activity_tx.clone();
        let spool_path = self.format_working_dir(
            &self
                .workspace_root
                .join(diapason_commons::workspace_dir_name())
                .join("context/tool_outputs")
                .join(format!("write_stdin_{session_id}.txt")),
        );
        let spool_file_path = self.workspace_root.join(&spool_path);
        let spool_ready = Arc::new(AtomicBool::new(false));
        let spool_ready_for_task = Arc::clone(&spool_ready);
        let spool_failed = Arc::new(AtomicBool::new(false));
        let spool_failed_for_task = Arc::clone(&spool_failed);
        let spool_finished = Arc::new(AtomicBool::new(false));
        let spool_finished_for_task = Arc::clone(&spool_finished);
        let output_task = tokio::spawn(async move {
            let mut spool_file =
                if tokio::fs::create_dir_all(spool_file_path.parent().unwrap_or_else(|| Path::new(".")))
                    .await
                    .is_ok()
                {
                    tokio::fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .open(&spool_file_path)
                        .await
                        .ok()
                } else {
                    None
                };
            if spool_file.is_none() {
                spool_failed_for_task.store(true, Ordering::Release);
            } else {
                spool_ready_for_task.store(true, Ordering::Release);
            }
            let mut spool_redactor = diapason_commons::sanitizer::StreamingSecretRedactor::default();
            loop {
                match tokio::time::timeout(tokio::time::Duration::from_millis(15), output_rx.recv()).await {
                    Ok(Some(chunk)) => {
                        // Keep the decoded text as a `Cow<str>` so that the
                        // common case (valid UTF-8) borrows `chunk` with zero
                        // allocation. `.into_owned()` would force a String copy
                        // on every chunk even when the bytes are already valid.
                        let text = String::from_utf8_lossy(&chunk);
                        if let Some(file) = spool_file.as_mut() {
                            let sanitized = spool_redactor.push(&text);
                            if !sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err() {
                                spool_failed_for_task.store(true, Ordering::Release);
                                spool_file = None;
                            }
                        }
                        output_clone.append(&text, chunk.len()).await;
                        output_activity_tx.send_modify(|version| *version += 1);
                    }
                    Ok(None) => break,
                    Err(_) if output_handle.has_exited() && output_handle.is_output_drained() => {
                        break;
                    }
                    Err(_) => continue,
                }
            }
            if let Some(file) = spool_file.as_mut() {
                let sanitized = spool_redactor.finish();
                if (!sanitized.is_empty() && file.write_all(sanitized.as_bytes()).await.is_err())
                    || file.flush().await.is_err()
                {
                    spool_failed_for_task.store(true, Ordering::Release);
                }
            }
            spool_finished_for_task.store(true, Ordering::Release);
        });
        let exit_rx = spawned.exit_rx;
        let exit_activity_tx = activity_tx.clone();
        let exit_task = tokio::spawn(async move {
            let _ = exit_rx.await;
            exit_activity_tx.send_modify(|version| *version += 1);
        });
        let record = Arc::new(PipeSessionRecord::new(
            metadata.clone(),
            handle,
            output,
            output_task,
            exit_task,
            activity_tx,
            PipeSpoolState {
                path: spool_path,
                ready: spool_ready,
                failed: spool_failed,
                finished: spool_finished,
            },
        ));

        sessions.insert(session_id, Arc::clone(&record));

        Ok((metadata, record))
    }

    /// Signals a session a cancelled create spawned but never dispatched.
    ///
    /// Runs from a drop handler, so it never awaits. The child is signalled
    /// first, which is what must not outlive the create. A child the backend
    /// could not terminate may still be running, so its record is kept: the map
    /// still names it and `close_session` can retry the kill. Withdrawing the
    /// record would leave the only remaining attempt to the single, unchecked
    /// kill in the process handle's drop, so a persistent failure would leave
    /// an untracked live child.
    ///
    /// A terminated child's backend record is withdrawn when its map lock is
    /// immediately available, so a later create under the same id does not
    /// collide with a dead record. If the lock is contended the record
    /// survives, and a later create under the id is refused as an existing
    /// session rather than signalling a process it does not own; the signalled
    /// child is already gone either way.
    fn discard_undispatched_session(&self, session_id: &str, record: &Arc<PipeSessionRecord>) {
        if self.terminate_record(session_id, record).is_err() {
            return;
        }
        if let Ok(mut sessions) = self.sessions.try_write() {
            if sessions.get(session_id).is_some_and(|current| Arc::ptr_eq(current, record)) {
                sessions.remove(session_id);
            }
        }
        if let Ok(mut output_task) = record.output_task.try_lock() {
            if let Some(task) = output_task.take() {
                task.abort();
            }
        }
        if let Ok(mut exit_task) = record.exit_task.try_lock() {
            if let Some(task) = exit_task.take() {
                task.abort();
            }
        }
    }

    async fn read_session_output(&self, session_id: &str, drain: bool) -> Result<Option<String>> {
        let record = self.session_record(session_id).await?;
        if drain {
            Ok(record.output.drain_pending().await)
        } else {
            Ok(record.output.peek_pending().await)
        }
    }

    async fn output_stats(&self, session_id: &str) -> Result<PipeOutputStats> {
        let record = self.session_record(session_id).await?;
        let (total_bytes, truncated) = record.output.stats().await;
        let spool_available =
            record.spool.ready.load(Ordering::Acquire) && !record.spool.failed.load(Ordering::Acquire);
        Ok(PipeOutputStats {
            total_bytes,
            truncated,
            spool_path: record.spool.path.clone(),
            spool_available,
            spool_complete: spool_available && record.spool.finished.load(Ordering::Acquire),
        })
    }

    async fn send_input_to_session(&self, session_id: &str, data: &[u8], append_newline: bool) -> Result<usize> {
        let record = self.session_record(session_id).await?;
        // A process that has already exited, or a writer task that has already
        // stopped, cannot accept input. Report this as the typed closed-writer
        // cause so the registry can classify it as a stale session without
        // claiming that any input was delivered.
        if record.handle.has_exited() || record.handle.is_writer_closed() {
            return Err(write_failure(session_id, false, None));
        }
        record
            .handle
            .write(data.to_vec())
            .await
            .map_err(|error| write_failure(session_id, false, Some(Box::new(error))))?;

        if append_newline {
            // The first write already reached the session, so a failure here
            // must keep the partial-state distinction.
            record
                .handle
                .write(b"\n".to_vec())
                .await
                .map_err(|error| write_failure(session_id, true, Some(Box::new(error))))?;
        }

        Ok(data.len() + usize::from(append_newline))
    }

    async fn is_session_completed(&self, session_id: &str) -> Result<Option<i32>> {
        let record = self.session_record(session_id).await?;
        if record.handle.has_exited() {
            Ok(record.handle.exit_code())
        } else {
            Ok(None)
        }
    }

    async fn terminate_session(&self, session_id: &str) -> Result<()> {
        let record = self.session_record(session_id).await?;
        record.handle.terminate();
        Ok(())
    }

    /// Removes and terminates a pipe-backed session.
    ///
    /// Termination runs before the record is removed, so a kill fault leaves
    /// the session resolvable and retryable instead of dropping the only handle
    /// to a process that may still be running. Termination is idempotent: a
    /// retry after an earlier successful kill observes an absent killer and
    /// succeeds, so a close that fails after the child was signalled cannot
    /// kill it twice.
    ///
    /// A missing identifier is reported through [`missing_session`]; callers
    /// surface that as a precise, non-mutating recovery hint.
    async fn close_session(&self, session_id: &str) -> Result<DiapasonExecSession> {
        let record = self.session_record(session_id).await?;
        self.terminate_record(session_id, &record)?;

        let record = {
            let mut sessions = self.sessions.write().await;
            sessions.remove(session_id).ok_or_else(|| missing_session(session_id))?
        };
        if let Some(task) = record.output_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = record.exit_task.lock().await.take() {
            task.abort();
        }

        Ok(record.metadata.clone())
    }

    /// Signals the session's child, reporting a kill fault.
    fn terminate_record(&self, session_id: &str, record: &PipeSessionRecord) -> Result<()> {
        #[cfg(test)]
        if let Some(message) = self
            .termination_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            return Err(anyhow!("failed to terminate exec session '{session_id}': {message}"));
        }

        record
            .handle
            .try_terminate()
            .with_context(|| format!("failed to terminate exec session '{session_id}'"))
    }

    /// Arms a one-shot kill fault for the next close-path termination.
    ///
    /// Test-only: it proves the close path keeps a session record retryable
    /// when the backend cannot signal the child. Its only caller spawns a real
    /// shell, so it is gated with that caller.
    #[cfg(all(test, unix))]
    fn fail_next_termination(&self, message: impl Into<String>) {
        *self.termination_failure.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message.into());
    }

    async fn activity_receiver(&self, session_id: &str) -> Result<watch::Receiver<u64>> {
        let record = self.session_record(session_id).await?;
        Ok(record.activity_tx.subscribe())
    }

    async fn is_output_drained(&self, session_id: &str) -> Result<bool> {
        let record = self.session_record(session_id).await?;
        let output_task = record.output_task.lock().await;
        let output_task_finished = match output_task.as_ref() {
            Some(task) => task.is_finished(),
            None => true,
        };
        Ok(record.handle.is_output_drained() && output_task_finished)
    }

    async fn terminate_all_sessions(&self) -> Result<()> {
        let ids = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect::<Vec<_>>()
        };

        for session_id in ids {
            self.close_session(&session_id).await?;
        }

        Ok(())
    }

    /// Looks up a live pipe session record by identifier.
    async fn session_record(&self, session_id: &str) -> Result<Arc<PipeSessionRecord>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned().ok_or_else(|| missing_session(session_id))
    }

    /// Rejects a path that escapes the session workspace root.
    fn ensure_within_workspace(&self, candidate: &Path) -> Result<()> {
        ensure_path_within_workspace(candidate, &self.workspace_root).map(|_| ())
    }

    fn format_working_dir(&self, path: &Path) -> String {
        match path.strip_prefix(&self.workspace_root) {
            Ok(relative) if relative.as_os_str().is_empty() => ".".into(),
            Ok(relative) => relative.to_string_lossy().replace("\\", "/"),
            Err(_) => path.to_string_lossy().into_owned(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecSessionBackend {
    Pipe,
    Pty,
}

struct ExecSessionRecord {
    metadata: DiapasonExecSession,
    backend: ExecSessionBackend,
    _pty_guard: Option<PtySessionGuard>,
}

impl ExecSessionRecord {
    fn new(metadata: DiapasonExecSession, backend: ExecSessionBackend, pty_guard: Option<PtySessionGuard>) -> Self {
        Self { metadata, backend, _pty_guard: pty_guard }
    }
}

/// Closes a backend session whose creation was cancelled before dispatch.
///
/// A create spawns the backend process and then awaits the dispatch map to
/// record it. A future dropped inside that await used to leave a live process
/// no dispatch entry named, so no close could ever reach it: `close_session`
/// starts from the dispatch map. The guard owns the spawned backend record
/// across the registration and closes it when the create does not complete. A
/// drop handler cannot await, so the child is signalled synchronously and the
/// backend record is withdrawn only when its lock is immediately available.
struct UndispatchedSessionGuard {
    exec_sessions: ExecSessionManager,
    session_id: ExecSessionId,
    backend: ExecSessionBackend,
    pipe_record: Option<Arc<PipeSessionRecord>>,
    armed: bool,
}

impl UndispatchedSessionGuard {
    /// Arms the guard for one pipe session the backend has already spawned.
    fn pipe(exec_sessions: ExecSessionManager, session_id: ExecSessionId, record: Arc<PipeSessionRecord>) -> Self {
        Self {
            exec_sessions,
            session_id,
            backend: ExecSessionBackend::Pipe,
            pipe_record: Some(record),
            armed: true,
        }
    }

    /// Arms the guard for one PTY session the backend has already created.
    fn pty(exec_sessions: ExecSessionManager, session_id: ExecSessionId) -> Self {
        Self {
            exec_sessions,
            session_id,
            backend: ExecSessionBackend::Pty,
            pipe_record: None,
            armed: true,
        }
    }

    /// Reports that the dispatch record now owns the session.
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for UndispatchedSessionGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        match self.backend {
            ExecSessionBackend::Pipe => {
                if let Some(record) = self.pipe_record.as_ref() {
                    self.exec_sessions
                        .pipe_sessions
                        .discard_undispatched_session(self.session_id.as_str(), record);
                }
            }
            ExecSessionBackend::Pty => {
                // The PTY backend's terminate is synchronous and idempotent, so
                // the child cannot outlive the cancelled create either.
                let _ = self
                    .exec_sessions
                    .pty_sessions
                    .manager()
                    .terminate_session(self.session_id.as_str());
            }
        }
    }
}

#[derive(Clone)]
pub struct ExecSessionManager {
    pipe_sessions: PipeSessionManager,
    pty_sessions: PtySessionManager,
    sessions: Arc<RwLock<HashMap<ExecSessionId, Arc<ExecSessionRecord>>>>,
    /// Per-session mutexes serializing creation against close.
    ///
    /// A close must not signal a backend record that a later create inserted
    /// under the same id, so both operations for one id hold this lock from the
    /// check through the map update. The map holds weak references and expired
    /// entries are pruned on every acquisition: a long-lived manager would
    /// otherwise retain a key and a mutex for every historical session, including
    /// failed creates. A strong reference held by an active operation or a
    /// waiter keeps its entry alive, so operations for one id still share a
    /// single lock.
    operation_locks: Arc<std::sync::Mutex<HashMap<ExecSessionId, std::sync::Weak<Mutex<()>>>>>,
    /// A scripted fault for the liveness read path.
    ///
    /// The background liveness projection must distinguish a genuinely absent
    /// session from any other read fault, and a live manager has no natural way
    /// to fail on demand. The seam exists only for this crate's tests and for
    /// the `test-support` feature, so production reads are unchanged.
    #[cfg(any(test, feature = "test-support"))]
    snapshot_failure: Arc<std::sync::Mutex<Option<String>>>,
}

impl ExecSessionManager {
    #[must_use]
    pub fn new(workspace_root: PathBuf, pty_sessions: PtySessionManager) -> Self {
        Self {
            pipe_sessions: PipeSessionManager::new(workspace_root),
            pty_sessions,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            operation_locks: Arc::new(std::sync::Mutex::new(HashMap::new())),
            #[cfg(any(test, feature = "test-support"))]
            snapshot_failure: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    /// Returns the per-session mutex that serializes create and close.
    ///
    /// Expired entries are pruned while the registry is locked, so a manager
    /// that has served many short-lived sessions does not keep a key, a mutex
    /// and their strings for every one of them. An entry is expired only once
    /// no operation holds or awaits its lock, so the surviving handle is the one
    /// every operation for that id shares.
    fn session_operation_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.operation_locks.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        let key: ExecSessionId = session_id.to_string().into();
        if let Some(lock) = locks.get(&key).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(key, Arc::downgrade(&lock));
        lock
    }

    /// Returns how many operation locks the registry still holds.
    ///
    /// The registry is process-wide state, so a test asserting that it was
    /// reclaimed must count only the ids it exercised.
    #[cfg(test)]
    fn operation_lock_count(&self) -> usize {
        self.operation_locks
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }

    /// Makes every subsequent snapshot read fail with `message`.
    ///
    /// Test-only: it lets a regression exercise the liveness projection's
    /// "read failed for another reason" branch without a real storage fault.
    /// A dependent crate's tests reach it through the `test-support` feature;
    /// a production build compiles neither this method nor the branch it arms.
    #[cfg(any(test, feature = "test-support"))]
    #[doc(hidden)]
    pub fn fail_snapshot_reads(&self, message: impl Into<String>) {
        *self.snapshot_failure.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(message.into());
    }

    pub(crate) async fn create_pipe_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
    ) -> Result<DiapasonExecSession> {
        self.create_pipe_session_with_sandbox(session_id, command, working_dir, env, false)
            .await
    }

    pub(crate) async fn create_pipe_session_with_sandbox(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        env: HashMap<String, String>,
        sandbox_active: bool,
    ) -> Result<DiapasonExecSession> {
        // Hold the per-session lock across check, spawn and dispatch insert so a
        // concurrent close cannot remove a record this create would replace, and
        // cannot terminate the session this create installs.
        let operation = self.session_operation_lock(session_id.as_str());
        let _operation = operation.lock().await;
        self.ensure_session_absent(&session_id).await?;
        let env = if sandbox_active {
            filter_sensitive_env(&env)
        } else {
            env
        };
        let (metadata, record) = self.pipe_sessions.create_session(session_id, command, working_dir, env).await?;
        // Armed across the dispatch registration, which is the last await of
        // the create. A caller dropped there still closes the process.
        let registration = UndispatchedSessionGuard::pipe(self.clone(), metadata.id.clone(), record);
        self.insert_session(metadata.clone(), ExecSessionBackend::Pipe, None).await?;
        registration.disarm();
        Ok(metadata)
    }

    pub(crate) async fn create_pty_session(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<ZshExecBridgeSession>,
    ) -> Result<DiapasonExecSession> {
        self.create_pty_session_with_sandbox(session_id, command, working_dir, size, extra_env, zsh_exec_bridge, false)
            .await
    }

    pub(crate) async fn create_pty_session_with_sandbox(
        &self,
        session_id: ExecSessionId,
        command: Vec<String>,
        working_dir: PathBuf,
        size: PtySize,
        extra_env: HashMap<String, String>,
        zsh_exec_bridge: Option<ZshExecBridgeSession>,
        sandbox_active: bool,
    ) -> Result<DiapasonExecSession> {
        // Hold the per-session lock across check, spawn and dispatch insert so a
        // concurrent close cannot remove a record this create would replace, and
        // cannot terminate the session this create installs.
        let operation = self.session_operation_lock(session_id.as_str());
        let _operation = operation.lock().await;
        self.ensure_session_absent(&session_id).await?;
        let pty_guard = self.pty_sessions.start_session()?;
        let metadata = self.pty_sessions.manager().create_session_with_bridge_sandboxed(
            session_id.clone().into(),
            command,
            working_dir,
            size,
            extra_env,
            zsh_exec_bridge,
            sandbox_active,
        )?;
        let exec_metadata = DiapasonExecSession::from(metadata);
        // Armed across the dispatch registration for the same reason the pipe
        // create arms it: a dropped create must not leave a live PTY child no
        // dispatch entry names.
        let registration = UndispatchedSessionGuard::pty(self.clone(), exec_metadata.id.clone());
        self.insert_session(exec_metadata.clone(), ExecSessionBackend::Pty, Some(pty_guard))
            .await?;
        registration.disarm();
        Ok(exec_metadata)
    }

    pub(crate) async fn snapshot_session(&self, session_id: &str) -> Result<DiapasonExecSession> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some(message) = self
            .snapshot_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            return Err(anyhow!("{message}"));
        }
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.session_record(session_id).await.map(|r| {
                let mut metadata = r.metadata.clone();
                let exit_code = if r.handle.has_exited() {
                    r.handle.exit_code()
                } else {
                    None
                };
                metadata.exit_code = exit_code;
                metadata.lifecycle_state = Some(if exit_code.is_some() {
                    crate::tools::types::DiapasonSessionLifecycleState::Exited
                } else {
                    crate::tools::types::DiapasonSessionLifecycleState::Running
                });
                metadata
            }),
            ExecSessionBackend::Pty => self
                .pty_sessions
                .manager()
                .snapshot_session(session_id)
                .map(DiapasonExecSession::from),
        }
    }

    pub(crate) async fn list_sessions(&self) -> Vec<DiapasonExecSession> {
        let sessions = self.sessions.read().await;
        let mut listed = sessions.values().map(|record| record.metadata.clone()).collect::<Vec<_>>();
        listed.sort_by(|left, right| left.id.cmp(&right.id));
        listed
    }

    pub(crate) async fn read_session_output(&self, session_id: &str, drain: bool) -> Result<Option<String>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.read_session_output(session_id, drain).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().read_session_output(session_id, drain),
        }
    }

    pub(crate) async fn output_stats(&self, session_id: &str) -> Result<Option<PipeOutputStats>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.output_stats(session_id).await.map(Some),
            ExecSessionBackend::Pty => self.pty_sessions.manager().output_stats(session_id).map(|stats| {
                stats.map(|stats| PipeOutputStats {
                    total_bytes: stats.total_bytes,
                    truncated: stats.truncated,
                    spool_path: stats.spool_path,
                    spool_available: stats.spool_available,
                    spool_complete: stats.spool_complete,
                })
            }),
        }
    }

    pub(crate) async fn send_input_to_session(
        &self,
        session_id: &str,
        data: &[u8],
        append_newline: bool,
    ) -> Result<usize> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => {
                self.pipe_sessions.send_input_to_session(session_id, data, append_newline).await
            }
            ExecSessionBackend::Pty => {
                self.pty_sessions
                    .manager()
                    .send_input_to_session(session_id, data, append_newline)
            }
        }
    }

    pub(crate) async fn is_session_completed(&self, session_id: &str) -> Result<Option<i32>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.is_session_completed(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().is_session_completed(session_id),
        }
    }

    pub(crate) async fn activity_receiver(&self, session_id: &str) -> Result<Option<watch::Receiver<u64>>> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.activity_receiver(session_id).await.map(Some),
            ExecSessionBackend::Pty => Ok(None),
        }
    }

    pub(crate) async fn is_output_drained(&self, session_id: &str) -> Result<bool> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.is_output_drained(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().is_output_drained(session_id),
        }
    }

    pub(crate) async fn terminate_session(&self, session_id: &str) -> Result<()> {
        let record = self.session_record(session_id).await?;
        match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.terminate_session(session_id).await,
            ExecSessionBackend::Pty => self.pty_sessions.manager().terminate_session(session_id),
        }
    }

    /// Removes and terminates a session, whatever its backend.
    ///
    /// The dispatch record is released only after the backend has terminated,
    /// so a kill fault leaves the session resolvable and retryable rather than
    /// dropping the dispatch entry for a process that may still be running.
    /// Termination is idempotent, so a retry after an earlier successful kill
    /// cannot kill the child twice.
    ///
    /// A backend that no longer holds a handle is equally complete. A close
    /// that is cancelled between closing the backend and removing the dispatch
    /// record leaves the record behind, and failing the retry on the backend's
    /// typed not-found would strand it — and the PTY guard it owns — forever.
    /// The typed missing cause is therefore the one fault that proceeds to the
    /// removal; every other fault keeps the record so the close stays
    /// retryable.
    ///
    /// The record read here is the only one this close may terminate. Both
    /// termination and removal run under the per-session lock that creation
    /// also holds, and the record is revalidated under that lock before the
    /// backend is touched. A close that read a record a concurrent close
    /// removed — and a create replaced under the same id — therefore reports
    /// the typed missing-session cause and leaves the replacement running
    /// instead of signalling it.
    ///
    /// A missing identifier is reported through [`missing_session`] rather
    /// than being treated as a successful no-op.
    pub(crate) async fn close_session(&self, session_id: &str) -> Result<DiapasonExecSession> {
        let record = self.session_record(session_id).await?;
        self.close_session_record(session_id, record).await
    }

    /// Closes the exact dispatch record the caller read.
    ///
    /// The record is revalidated under the session's create/close lock, so a
    /// record that a concurrent close removed — or a same-id create replaced —
    /// is refused without touching the backend. The caller receives
    /// [`missing_session`] and the replacement keeps running.
    async fn close_session_record(
        &self,
        session_id: &str,
        record: Arc<ExecSessionRecord>,
    ) -> Result<DiapasonExecSession> {
        let operation = self.session_operation_lock(session_id);
        let _operation = operation.lock().await;
        let current = self.session_record(session_id).await?;
        if !Arc::ptr_eq(&current, &record) {
            return Err(missing_session(session_id));
        }

        let closed_metadata = match record.backend {
            ExecSessionBackend::Pipe => self.pipe_sessions.close_session(session_id).await,
            ExecSessionBackend::Pty => self
                .pty_sessions
                .manager()
                .close_session(session_id)
                .map(DiapasonExecSession::from),
        };
        let metadata = match closed_metadata {
            Ok(metadata) => metadata,
            // The backend handle is already gone: that is where an interrupted
            // close stops, so finish the removal the retry is here for.
            Err(error) if is_missing_exec_session(&error) => record.metadata.clone(),
            Err(error) => return Err(error),
        };

        {
            let mut sessions = self.sessions.write().await;
            // Only the record read above may be released: a session that was
            // closed and re-created under the same id keeps its own guard. The
            // per-session lock already excludes a replacement, so this remains
            // a last defence rather than the rule that enforces it.
            if !sessions.get(session_id).is_some_and(|current| Arc::ptr_eq(current, &record)) {
                return Err(missing_session(session_id));
            }
            sessions.remove(session_id).ok_or_else(|| missing_session(session_id))?;
        }

        Ok(metadata)
    }

    pub(crate) async fn prune_exited_session(&self, session_id: &str) -> Result<Option<DiapasonExecSession>> {
        if self.is_session_completed(session_id).await?.is_some() {
            return self.close_session(session_id).await.map(Some);
        }
        Ok(None)
    }

    pub(crate) async fn terminate_all_sessions_async(&self) -> Result<()> {
        let ids = {
            let sessions = self.sessions.read().await;
            sessions.keys().cloned().collect::<Vec<_>>()
        };

        let mut failures = Vec::new();
        for session_id in ids {
            if let Err(err) = self.close_session(&session_id).await {
                failures.push(format!("{session_id}: {err}"));
            }
        }

        if let Err(err) = self.pipe_sessions.terminate_all_sessions().await {
            failures.push(err.to_string());
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("failed to terminate all exec sessions: {}", failures.join("; ")))
        }
    }

    async fn insert_session(
        &self,
        metadata: DiapasonExecSession,
        backend: ExecSessionBackend,
        pty_guard: Option<PtySessionGuard>,
    ) -> Result<()> {
        #[cfg(all(test, unix))]
        pass_session_registration_gate(&self.sessions, metadata.id.as_str()).await;
        let mut sessions = self.sessions.write().await;
        use hashbrown::hash_map::Entry;
        match sessions.entry(metadata.id.clone()) {
            Entry::Occupied(_) => Err(anyhow!("exec session '{}' already exists", metadata.id.as_str())),
            Entry::Vacant(entry) => {
                entry.insert(Arc::new(ExecSessionRecord::new(metadata, backend, pty_guard)));
                Ok(())
            }
        }
    }

    async fn ensure_session_absent(&self, session_id: &str) -> Result<()> {
        let sessions = self.sessions.read().await;
        if sessions.contains_key(session_id) {
            return Err(anyhow!("exec session '{session_id}' already exists"));
        }
        Ok(())
    }

    /// Looks up a live exec session record by identifier.
    async fn session_record(&self, session_id: &str) -> Result<Arc<ExecSessionRecord>> {
        let sessions = self.sessions.read().await;
        sessions.get(session_id).cloned().ok_or_else(|| missing_session(session_id))
    }
}

/// A test gate between a spawned backend session and its dispatch record.
///
/// A create spawns the process in its backend and then awaits the dispatch map
/// under an async lock. No real scheduling makes a caller drop inside that
/// await deterministic, so a test arms this gate to cancel the create exactly
/// there and observe what happens to the process.
#[cfg(all(test, unix))]
struct SessionRegistrationGate {
    sessions: std::sync::Weak<RwLock<HashMap<ExecSessionId, Arc<ExecSessionRecord>>>>,
    session_id: String,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[cfg(all(test, unix))]
fn session_registration_gates() -> &'static std::sync::Mutex<Vec<SessionRegistrationGate>> {
    static GATES: std::sync::OnceLock<std::sync::Mutex<Vec<SessionRegistrationGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// Arms a gate that parks the next create of `session_id` before dispatch.
#[cfg(all(test, unix))]
pub(crate) fn gate_next_session_registration(
    exec_sessions: &ExecSessionManager,
    session_id: &str,
) -> (Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>) {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let mut gates = session_registration_gates()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    gates.retain(|gate| gate.sessions.strong_count() > 0);
    gates.push(SessionRegistrationGate {
        sessions: Arc::downgrade(&exec_sessions.sessions),
        session_id: session_id.to_owned(),
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    });
    (entered, release)
}

/// Parks a create here when the gate armed for its session and manager matches.
#[cfg(all(test, unix))]
async fn pass_session_registration_gate(
    sessions: &Arc<RwLock<HashMap<ExecSessionId, Arc<ExecSessionRecord>>>>,
    session_id: &str,
) {
    let gate = {
        let mut gates = session_registration_gates()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        gates.retain(|gate| gate.sessions.strong_count() > 0);
        gates
            .iter()
            .position(|gate| {
                gate.session_id == session_id && std::ptr::eq(gate.sessions.as_ptr(), Arc::as_ptr(sessions))
            })
            .map(|index| gates.remove(index))
    };
    let Some(gate) = gate else {
        return;
    };
    gate.entered.notify_one();
    gate.release.notified().await;
}

#[cfg(all(test, unix))]
mod tests {
    use std::sync::Arc;

    use hashbrown::HashMap;
    use tempfile::tempdir;
    use tokio::sync::watch;
    use tokio::time::{Duration, timeout};

    use super::{
        ExecSessionLostAfterInput, ExecSessionManager, ExecSessionNotFound, ExecSessionWriterClosed, write_failure,
    };
    use crate::config::PtyConfig;
    use crate::tools::pty::{PtySessionNotFound, PtySize};
    use crate::tools::registry::PtySessionManager;
    use crate::utils::path::canonicalize_workspace;

    /// A create cancelled before its dispatch record closes the child it spawned.
    ///
    /// The pipe backend spawns the process and stores it before the dispatch
    /// record is inserted under an async lock. A caller dropped inside that
    /// await leaves a live child no dispatch entry names, so `close_session`
    /// could never reach it. The gate parks the create there; the guard must
    /// signal the child and release the backend record, which re-creating the
    /// same id proves.
    #[tokio::test]
    async fn a_create_cancelled_before_dispatch_closes_the_child_it_spawned() {
        let workspace = tempdir().expect("workspace");
        let manager = ExecSessionManager::new(
            workspace.path().to_path_buf(),
            PtySessionManager::new(workspace.path().to_path_buf(), PtyConfig::default()),
        );
        let (entered, _release) = super::gate_next_session_registration(&manager, "exec-cancelled");
        {
            let creator = manager.clone();
            let create = creator.create_pipe_session(
                "exec-cancelled".to_string().into(),
                vec!["sh".to_string(), "-c".to_string(), "sleep 30".to_string()],
                workspace.path().to_path_buf(),
                HashMap::new(),
            );
            tokio::pin!(create);
            tokio::select! {
                result = &mut create => panic!("the gated create registered before it was cancelled: {result:?}"),
                () = entered.notified() => {}
            }
            // Dropping the pinned future here cancels the create inside its
            // dispatch registration, after the backend spawned the process.
        }
        // Re-creating the id succeeds only when the cancelled create withdrew
        // its backend record, and that path terminates the child first.
        manager
            .create_pipe_session(
                "exec-cancelled".to_string().into(),
                vec!["sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace.path().to_path_buf(),
                HashMap::new(),
            )
            .await
            .expect("the cancelled create released its session id");
        assert!(manager.snapshot_session("exec-cancelled").await.is_ok(), "the replacement session is registered");
        manager.terminate_all_sessions_async().await.expect("close the replacement");
    }

    /// Polls the session's activity channel until output appears or times out.
    async fn wait_for_pipe_output(
        manager: &ExecSessionManager,
        session_id: &str,
        activity_rx: &mut watch::Receiver<u64>,
    ) -> anyhow::Result<String> {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Some(output) = manager.read_session_output(session_id, true).await? {
                    return Ok::<String, anyhow::Error>(output);
                }
                activity_rx.changed().await?;
            }
        })
        .await?
    }

    /// Reports a missing session as the typed `ExecSessionNotFound` error.
    #[tokio::test]
    async fn missing_session_preserves_typed_identity() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root, pty_sessions);

        let error = manager
            .read_session_output("run-stale", true)
            .await
            .expect_err("unknown session should fail");
        let missing = error
            .downcast_ref::<ExecSessionNotFound>()
            .expect("missing session should retain its typed cause");

        assert_eq!(missing.session_id, "run-stale");
        assert_eq!(error.to_string(), "exec session 'run-stale' not found");
        Ok(())
    }

    /// Reports a terminated pipe writer as the typed closed-writer cause.
    #[tokio::test]
    #[cfg(unix)]
    async fn send_input_to_terminated_pipe_session_reports_closed_writer() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "writer-closed".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        // Terminating the session aborts the writer task without removing the
        // session record, so a later write must fail as a closed writer rather
        // than as a missing session.
        manager.terminate_session("writer-closed").await?;
        let record = manager.pipe_sessions.session_record("writer-closed").await?;
        timeout(Duration::from_secs(5), async {
            while !record.handle.has_exited() && !record.handle.is_writer_closed() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;

        let error = manager
            .send_input_to_session("writer-closed", b"input", false)
            .await
            .expect_err("a terminated pipe writer must reject input");
        let closed = error
            .downcast_ref::<ExecSessionWriterClosed>()
            .expect("closed writer should retain its typed cause");
        assert_eq!(closed.session_id, "writer-closed");
        assert!(
            error.downcast_ref::<ExecSessionNotFound>().is_none(),
            "a closed writer must stay distinct from a missing session"
        );

        manager.close_session("writer-closed").await?;
        Ok(())
    }

    /// Keeps the post-delivery cause distinct from the pre-delivery cause.
    #[test]
    fn write_failure_keeps_partial_state_distinction() {
        let before = write_failure("run-1", false, None);
        let before = before
            .downcast_ref::<ExecSessionWriterClosed>()
            .expect("a pre-delivery failure is a closed writer");
        assert_eq!(before.session_id, "run-1");

        let after = write_failure("run-1", true, None);
        let after = after
            .downcast_ref::<ExecSessionLostAfterInput>()
            .expect("a post-delivery failure lost the session after input");
        assert_eq!(after.session_id, "run-1");
    }

    #[tokio::test]
    #[cfg(all(unix, feature = "tui"))]
    async fn pty_session_limit_holds_until_exec_session_close() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions =
            PtySessionManager::new(workspace_root.clone(), PtyConfig { max_sessions: 1, ..Default::default() });
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        manager
            .create_pty_session(
                "run-1".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root.clone(),
                size,
                HashMap::new(),
                None,
            )
            .await?;

        let second = manager
            .create_pty_session(
                "run-2".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root.clone(),
                size,
                HashMap::new(),
                None,
            )
            .await;
        assert!(second.is_err());
        assert!(second.unwrap_err().to_string().contains("Maximum PTY sessions"));

        manager.close_session("run-1").await?;
        manager
            .create_pty_session(
                "run-3".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 1".to_string()],
                workspace_root,
                size,
                HashMap::new(),
                None,
            )
            .await?;
        manager.close_session("run-3").await?;

        Ok(())
    }

    /// A close interrupted after the backend released its handle still
    /// finishes, releasing the PTY guard the dispatch record owns.
    ///
    /// The close terminates the backend first and removes the dispatch record
    /// second. A cancellation in that window leaves the record behind, and a
    /// retry that stopped at the backend's typed not-found would report a fault
    /// for a session that is already closed and hold the PTY slot forever.
    #[tokio::test]
    #[cfg(all(unix, feature = "tui"))]
    async fn close_session_finishes_after_the_backend_released_its_handle() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        manager
            .create_pty_session(
                "close-interrupted".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root,
                size,
                HashMap::new(),
                None,
            )
            .await?;
        assert_eq!(pty_sessions.active_sessions(), 1, "the open session holds one PTY guard");

        // Release only the backend handle, exactly the window `close_session`
        // opens between closing the backend and removing the dispatch record.
        pty_sessions.manager().close_session("close-interrupted")?;
        assert_eq!(
            pty_sessions.active_sessions(),
            1,
            "the interrupted close leaves the dispatch record, and its guard, behind"
        );

        let metadata = manager.close_session("close-interrupted").await?;
        assert_eq!(metadata.id.as_str(), "close-interrupted");
        assert_eq!(metadata.backend, "pty", "the recovered close must keep the session's backend");
        assert_eq!(
            pty_sessions.active_sessions(),
            0,
            "the retried close must release the PTY guard the dispatch record owned"
        );
        assert!(manager.list_sessions().await.is_empty(), "the retried close must remove the record");
        Ok(())
    }

    /// Classifies a backend that dropped its handle after the record was read.
    ///
    /// Closing a session terminates and releases the backend handle first and
    /// removes the dispatch record second, so a concurrent read can find the
    /// record and still miss the handle. That race must surface as the typed
    /// missing-session cause, so the background liveness projection treats it
    /// as an expected exit rather than logging an unobserved fault that quotes
    /// the session id.
    #[tokio::test]
    #[cfg(all(unix, feature = "tui"))]
    async fn snapshot_after_the_backend_dropped_the_handle_reports_a_missing_session() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
        let size = PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        };

        manager
            .create_pty_session(
                "run-race".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root,
                size,
                HashMap::new(),
                None,
            )
            .await?;

        // Release only the backend handle, exactly the window `close_session`
        // opens between terminating the process and removing the dispatch
        // record.
        pty_sessions.manager().close_session("run-race")?;

        let error = manager
            .snapshot_session("run-race")
            .await
            .expect_err("a record whose backend handle is gone must not read as alive");
        assert!(
            error.chain().any(|cause| cause.is::<PtySessionNotFound>()),
            "the race must retain the typed missing-session cause: {error}"
        );
        assert!(
            error.downcast_ref::<ExecSessionNotFound>().is_none(),
            "the dispatch record still exists, so the failure must not claim the record is missing"
        );

        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_activity_receiver_notifies_on_output() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "run-1".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "printf hello".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("run-1")
            .await?
            .expect("pipe sessions should expose activity receiver");

        let output = wait_for_pipe_output(&manager, "run-1", &mut activity_rx).await?;
        assert!(output.contains("hello"));

        manager.close_session("run-1").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn concurrent_pipe_session_create_with_same_id_creates_exactly_one() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        let (a, b) = tokio::join!(
            manager.create_pipe_session(
                "same-id".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root.clone(),
                HashMap::new(),
            ),
            manager.create_pipe_session(
                "same-id".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root.clone(),
                HashMap::new(),
            ),
        );

        assert_eq!(a.is_ok() as u8 + b.is_ok() as u8, 1, "exactly one concurrent create must win: {a:?} {b:?}");
        let loser = a.err().or_else(|| b.err()).expect("the loser should error");
        assert!(loser.to_string().contains("already exists"), "loser error should report duplicate: {loser}");

        manager.close_session("same-id").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_drain_clears_so_old_output_does_not_reappear() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "drain-clear".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "printf hello".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("drain-clear")
            .await?
            .ok_or_else(|| anyhow::anyhow!("pipe session drain-clear should expose an activity receiver"))?;

        let drained = wait_for_pipe_output(&manager, "drain-clear", &mut activity_rx).await?;
        anyhow::ensure!(drained.contains("hello"), "drained pipe output should contain hello; observed {drained:?}");

        let stale = manager.read_session_output("drain-clear", true).await?;
        anyhow::ensure!(stale.is_none(), "drained output must not reappear: {stale:?}");

        manager.close_session("drain-clear").await?;
        Ok(())
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn pipe_session_returns_new_output_after_drain() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "drain-resume".to_string().into(),
                vec![
                    "/bin/sh".to_string(),
                    "-c".to_string(),
                    "printf one; sleep 1; printf two".to_string(),
                ],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let mut activity_rx = manager
            .activity_receiver("drain-resume")
            .await?
            .expect("pipe sessions should expose activity receiver");

        let _first = wait_for_pipe_output(&manager, "drain-resume", &mut activity_rx).await?;

        let second = wait_for_pipe_output(&manager, "drain-resume", &mut activity_rx).await?;
        assert!(second.contains("two"), "output produced after a drain must still be returned: {second:?}");

        manager.close_session("drain-resume").await?;
        Ok(())
    }

    #[tokio::test]
    async fn pipe_output_buffer_peek_is_idempotent_and_non_consuming() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        let first = buffer.peek_pending().await;
        let second = buffer.peek_pending().await;
        assert_eq!(first, second);
        assert_eq!(first, Some("hello".to_string()));
    }

    #[tokio::test]
    async fn pipe_output_buffer_drain_returns_exactly_once() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        let first = buffer.drain_pending().await;
        let second = buffer.drain_pending().await;
        assert_eq!(first, Some("hello".to_string()));
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn pipe_output_buffer_drain_clears_internal_pending_length() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("hello", 5).await;

        buffer.drain_pending().await;
        let peek: Option<String> = buffer.peek_pending().await;
        assert!(peek.is_none(), "buffer must be empty after drain: {peek:?}");
    }

    #[tokio::test]
    async fn pipe_output_buffer_append_after_drain_returns_only_fresh_output() {
        let buffer = super::PipeOutputBuffer::default();
        buffer.append("first", 5).await;
        buffer.drain_pending().await;

        buffer.append("second", 6).await;
        let output = buffer.peek_pending().await;
        assert_eq!(output, Some("second".to_string()));
    }

    #[tokio::test]
    async fn pipe_output_buffer_bounds_preview_and_tracks_total_bytes() {
        let buffer = super::PipeOutputBuffer::default();
        let chunk = "x".repeat(super::PIPE_OUTPUT_HEAD_BYTES * 4);
        buffer.append(&chunk, chunk.len()).await;

        let preview = buffer.peek_pending().await.expect("bounded preview");
        assert!(preview.len() <= super::PIPE_OUTPUT_HEAD_BYTES * 3);
        let (total_bytes, truncated) = buffer.stats().await;
        assert_eq!(total_bytes, chunk.len() as u64);
        assert!(truncated);
    }

    /// A normal close terminates the child, removes the record and returns its
    /// metadata.
    #[tokio::test]
    #[cfg(unix)]
    async fn close_session_removes_the_record_and_returns_metadata() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "close-normal".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let metadata = manager.close_session("close-normal").await?;
        assert_eq!(metadata.id.as_str(), "close-normal");
        assert_eq!(metadata.backend, "pipe");

        let error = manager
            .snapshot_session("close-normal")
            .await
            .expect_err("a closed session must no longer resolve");
        assert!(
            error.downcast_ref::<ExecSessionNotFound>().is_some(),
            "the removed record must report the typed missing-session cause: {error}"
        );
        assert!(manager.list_sessions().await.is_empty(), "the closed record must not be listed");
        Ok(())
    }

    /// A failed kill must keep the record reachable so a retry can finish the
    /// close instead of abandoning a live process.
    #[tokio::test]
    #[cfg(unix)]
    async fn close_session_keeps_the_record_when_termination_fails() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "close-fault".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 5".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        manager.pipe_sessions.fail_next_termination("simulated kill failure");

        let error = manager
            .close_session("close-fault")
            .await
            .expect_err("a kill fault must surface from close_session");
        assert!(
            format!("{error:#}").contains("simulated kill failure"),
            "the injected fault must be reported: {error:#}"
        );

        // The dispatch and backend records must both survive so a retry can
        // still reach the process.
        manager.session_record("close-fault").await?;
        manager.pipe_sessions.session_record("close-fault").await?;
        let retained = manager.snapshot_session("close-fault").await?;
        assert_eq!(retained.id.as_str(), "close-fault");

        // The one-shot fault has cleared, so the retry completes the close.
        let metadata = manager.close_session("close-fault").await?;
        assert_eq!(metadata.id.as_str(), "close-fault");

        let error = manager
            .snapshot_session("close-fault")
            .await
            .expect_err("the retried close must remove the record");
        assert!(
            error.downcast_ref::<ExecSessionNotFound>().is_some(),
            "the retried close must report the typed missing-session cause: {error}"
        );
        Ok(())
    }

    /// A close bound to a removed record must not kill its same-id replacement.
    ///
    /// A close reads its dispatch record before it terminates the backend. If
    /// that record is closed and a replacement is launched under the same id in
    /// the meantime, terminating by the textual id would signal the replacement
    /// while the later identity check rejects the stale removal. Termination
    /// now runs under the per-session lock and revalidates the record, so the
    /// stale close reports the typed missing-session cause and the replacement
    /// keeps running.
    #[tokio::test]
    #[cfg(unix)]
    async fn a_stale_close_after_a_same_id_recreate_leaves_the_replacement_running() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let workspace_root = canonicalize_workspace(temp_dir.path());
        let pty_sessions = PtySessionManager::new(workspace_root.clone(), PtyConfig::default());
        let manager = ExecSessionManager::new(workspace_root.clone(), pty_sessions);

        manager
            .create_pipe_session(
                "recreated".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 30".to_string()],
                workspace_root.clone(),
                HashMap::new(),
            )
            .await?;
        // The record a close would have read before the replacement existed.
        let stale = manager.session_record("recreated").await?;

        manager.close_session("recreated").await?;
        manager
            .create_pipe_session(
                "recreated".to_string().into(),
                vec!["/bin/sh".to_string(), "-c".to_string(), "sleep 30".to_string()],
                workspace_root,
                HashMap::new(),
            )
            .await?;

        let error = manager
            .close_session_record("recreated", stale)
            .await
            .expect_err("a close bound to the removed record must not touch the replacement");
        assert!(
            error.downcast_ref::<ExecSessionNotFound>().is_some(),
            "the stale close must report the typed missing-session cause: {error}"
        );

        let replacement = manager.pipe_sessions.session_record("recreated").await?;
        assert!(!replacement.handle.has_exited(), "the replacement process must keep running");
        let snapshot = manager.snapshot_session("recreated").await?;
        assert_eq!(
            snapshot.lifecycle_state,
            Some(crate::tools::types::DiapasonSessionLifecycleState::Running),
            "the replacement must still be the live session under the id"
        );

        manager.close_session("recreated").await?;
        Ok(())
    }
    /// Operation locks are reclaimed once no operation can still hold them.
    ///
    /// Every create and close asks the registry for the same per-id lock. A
    /// long-lived manager used to retain one entry, with its key and mutex, for
    /// every session it had ever served, including failed creates. An expired
    /// entry must be pruned, while a lock an operation still holds is shared by
    /// every other operation for that id.
    #[tokio::test]
    async fn operation_locks_are_reclaimed_after_the_last_user_releases_them() {
        let workspace = tempdir().expect("workspace");
        let manager = ExecSessionManager::new(
            workspace.path().to_path_buf(),
            PtySessionManager::new(workspace.path().to_path_buf(), PtyConfig::default()),
        );

        let first = manager.session_operation_lock("exec-reclaimed");
        let waiter = manager.session_operation_lock("exec-reclaimed");
        assert!(Arc::ptr_eq(&first, &waiter), "one id must share one lock while either handle lives");
        drop(first);
        drop(waiter);

        // A different id must not retain the expired entry beside its own.
        let _other = manager.session_operation_lock("exec-other");
        assert_eq!(
            manager.operation_lock_count(),
            1,
            "an expired operation lock must be pruned instead of retained for the process lifetime"
        );
    }
}
