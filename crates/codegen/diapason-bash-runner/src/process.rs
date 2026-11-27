//! Unified process handle types for PTY and pipe backends.
//!
//! This module provides abstractions for interacting with spawned processes
//! regardless of whether they use a PTY or regular pipes.
//!
//! Inspired by [codex-rs] PTY process handle patterns (Apache-2.0).
//! Copyright 2025 OpenAI. See the repository `THIRD-PARTY-NOTICES` file for
//! full attribution.
//!
//! [codex-rs]: https://github.com/openai/codex
//!
//! ## Async-drop pattern
//!
//! `Drop` cannot be `async`, but some cleanup requires async operations
//! (e.g. asking a runtime to stop a process, removing a container, closing
//! a network connection). The pattern borrowed from `testcontainers-rs` is to
//! spin a dedicated thread inside `Drop`, create a temporary Tokio runtime on
//! that thread, and block the `Drop` call until the future resolves. This is
//! heavier than a true `async drop`, but it lets us bridge sync `Drop` into
//! async cleanup without requiring nightly Rust.

use std::fmt;
use std::io;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

use bytes::Bytes;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::{AbortHandle, JoinHandle};

const POST_EXIT_DRAIN_QUIET_MS: u64 = 50;
const POST_EXIT_DRAIN_MAX_MS: u64 = 500;

/// Run an async cleanup future from synchronous `Drop`.
///
/// This bridges the gap between sync `Drop` and async resource cleanup.
/// A dedicated thread is spawned with its own Tokio runtime so the future
/// can make full use of async APIs. The `Drop` call blocks until the runtime
/// shuts down, giving us deterministic cleanup semantics similar to RAII.
///
/// Borrowed from the `testcontainers-rs` pattern for async-drop in Rust
/// (where true `async drop` is still nightly-only).
pub(crate) fn async_drop<F, Fut>(f: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let handle = std::thread::spawn(move || {
        let rt = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(_) => return,
        };
        rt.block_on(f());
    });
    let _ = handle.join();
}

/// Trait for process termination strategies.
///
/// Different backends (PTY vs pipe) may need different termination approaches.
pub trait ChildTerminator: Send + Sync {
    /// Kill the child process.
    fn kill(&mut self) -> io::Result<()>;
}

/// Optional PTY-specific handles that must be preserved.
///
/// For PTY processes, the slave handle must be kept alive because the process
/// will receive SIGHUP if it's closed.
pub struct PtyHandles {
    /// The slave PTY handle (kept alive to prevent SIGHUP).
    pub _slave: Option<Box<dyn Send>>,
    /// The master PTY handle.
    pub _master: Box<dyn Send>,
}

impl fmt::Debug for PtyHandles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PtyHandles").finish()
    }
}

/// Handle for driving an interactive or non-interactive process.
///
/// This provides a unified interface for both PTY and pipe-based processes:
/// - Write to stdin via `writer_sender()`
/// - Read merged stdout/stderr via `output_receiver()`
/// - Check exit status via `has_exited()` and `exit_code()`
/// - Clean up via `terminate()`
pub struct ProcessHandle {
    writer_tx: mpsc::Sender<Vec<u8>>,
    output_tx: broadcast::Sender<Bytes>,
    killer: StdMutex<Option<Box<dyn ChildTerminator>>>,
    reader_handle: StdMutex<Option<JoinHandle<()>>>,
    reader_abort_handles: StdMutex<Vec<AbortHandle>>,
    writer_handle: StdMutex<Option<JoinHandle<()>>>,
    wait_handle: StdMutex<Option<JoinHandle<()>>>,
    exit_status: Arc<AtomicBool>,
    exit_code: Arc<StdMutex<Option<i32>>>,
    // PTY handles must be preserved to prevent the process from receiving Control+C
    _pty_handles: StdMutex<Option<PtyHandles>>,
}

impl fmt::Debug for ProcessHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessHandle")
            .field("has_exited", &self.has_exited())
            .field("exit_code", &self.exit_code())
            .finish()
    }
}

impl ProcessHandle {
    /// Create a new process handle with all required components.
    #[allow(
        clippy::too_many_arguments,
        reason = "Intentional compatibility, platform, or test-only suppression."
    )]
    pub(crate) fn new(
        writer_tx: mpsc::Sender<Vec<u8>>,
        output_tx: broadcast::Sender<Bytes>,
        initial_output_rx: broadcast::Receiver<Bytes>,
        killer: Box<dyn ChildTerminator>,
        reader_handle: JoinHandle<()>,
        reader_abort_handles: Vec<AbortHandle>,
        writer_handle: JoinHandle<()>,
        wait_handle: JoinHandle<()>,
        exit_status: Arc<AtomicBool>,
        exit_code: Arc<StdMutex<Option<i32>>>,
        pty_handles: Option<PtyHandles>,
    ) -> (Self, broadcast::Receiver<Bytes>) {
        (
            Self {
                writer_tx,
                output_tx,
                killer: StdMutex::new(Some(killer)),
                reader_handle: StdMutex::new(Some(reader_handle)),
                reader_abort_handles: StdMutex::new(reader_abort_handles),
                writer_handle: StdMutex::new(Some(writer_handle)),
                wait_handle: StdMutex::new(Some(wait_handle)),
                exit_status,
                exit_code,
                _pty_handles: StdMutex::new(pty_handles),
            },
            initial_output_rx,
        )
    }

    /// Returns a channel sender for writing raw bytes to the child stdin.
    ///
    /// # Example
    /// ```ignore
    /// let writer = handle.writer_sender();
    /// writer.send(b"input\n".to_vec()).await?;
    /// ```
    #[inline]
    pub fn writer_sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.writer_tx.clone()
    }

    /// Returns a broadcast receiver that yields stdout/stderr chunks.
    ///
    /// Multiple receivers can be created; each receives all output from the
    /// point of subscription.
    #[inline]
    pub fn output_receiver(&self) -> broadcast::Receiver<Bytes> {
        self.output_tx.subscribe()
    }

    /// True if the child process has exited.
    #[inline]
    pub fn has_exited(&self) -> bool {
        self.exit_status.load(Ordering::SeqCst)
    }

    /// Returns the exit code if the process has exited.
    #[inline]
    pub fn exit_code(&self) -> Option<i32> {
        *self.exit_code.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// True once the stdout/stderr reader task has drained the child streams.
    #[inline]
    pub fn is_output_drained(&self) -> bool {
        self.reader_handle
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(JoinHandle::is_finished))
            .unwrap_or(true)
    }

    /// Attempts to kill the child and abort helper tasks.
    ///
    /// Best-effort wrapper around [`Self::try_terminate`]. A termination fault
    /// is reported to standard error using this module's `[diapason]` logging
    /// convention and then swallowed, because `terminate()` is infallible for
    /// its existing callers. The crate has no logging dependency, so the
    /// swallow is deliberate and documented here rather than delegated to a
    /// logger.
    ///
    /// This is idempotent and safe to call multiple times.
    pub fn terminate(&self) {
        if let Err(error) = self.try_terminate() {
            eprintln!(
                "[diapason] failed to terminate process (exited={}, exit_code={:?}): {error}",
                self.has_exited(),
                self.exit_code()
            );
        }
    }

    /// Terminates the child process, aborting its helper tasks once it is gone,
    /// and reports a kill fault.
    ///
    /// The killer is taken out of its lock so that termination cannot race a
    /// second call. If the child was already terminated, or a previous call
    /// already consumed the killer, this returns `Ok(())` exactly as the
    /// best-effort path did. A kill that reports the child is already gone is
    /// also success: the terminal state is what the caller asked for.
    ///
    /// A failed [`ChildTerminator::kill`] restores the killer and returns the
    /// error without aborting the helper tasks, so a caller that keeps a handle
    /// (for example a session record) can retry instead of stranding a process
    /// it can no longer signal. The helpers stay running on that path because a
    /// failed kill says nothing about the child: the wait task must still record
    /// a later exit, and the reader and writer must still drain its streams.
    /// Helper tasks are aborted only once termination succeeds or the child is
    /// confirmed already gone.
    ///
    /// This is idempotent: once the child has been terminated, later calls
    /// observe an absent killer and succeed without signalling again.
    pub fn try_terminate(&self) -> io::Result<()> {
        self.kill_child()?;
        self.abort_tasks();
        Ok(())
    }

    /// Signals the child, restoring the killer if signalling fails.
    fn kill_child(&self) -> io::Result<()> {
        let mut killer_opt = self.killer.lock().unwrap_or_else(|error| error.into_inner());
        let Some(mut killer) = killer_opt.take() else {
            return Ok(());
        };

        match killer.kill() {
            Ok(()) => Ok(()),
            Err(error) if is_already_gone(&error) => Ok(()),
            Err(error) => {
                // Restore the killer so a later retry can still reach the
                // child. A failed signal must not leave an unrecorded process
                // alive with no handle left to terminate it.
                if killer_opt.is_none() {
                    *killer_opt = Some(killer);
                }
                Err(error)
            }
        }
    }

    /// Abort all background tasks associated with this process.
    fn abort_tasks(&self) {
        // Abort reader handle
        if let Ok(mut h) = self.reader_handle.lock()
            && let Some(handle) = h.take()
        {
            handle.abort();
        }

        // Abort individual reader abort handles
        if let Ok(mut handles) = self.reader_abort_handles.lock() {
            for handle in handles.drain(..) {
                handle.abort();
            }
        }

        // Abort writer handle
        if let Ok(mut h) = self.writer_handle.lock()
            && let Some(handle) = h.take()
        {
            handle.abort();
        }

        // Abort wait handle
        if let Ok(mut h) = self.wait_handle.lock()
            && let Some(handle) = h.take()
        {
            handle.abort();
        }
    }

    /// Check if the process is still running.
    #[inline]
    pub fn is_running(&self) -> bool {
        !self.has_exited() && !self.is_writer_closed()
    }

    /// Send bytes to the process stdin.
    ///
    /// Returns an error if the stdin channel is closed.
    pub async fn write(&self, bytes: impl Into<Vec<u8>>) -> Result<(), mpsc::error::SendError<Vec<u8>>> {
        self.writer_tx.send(bytes.into()).await
    }

    /// Check if the writer channel is closed.
    #[inline]
    pub fn is_writer_closed(&self) -> bool {
        self.writer_tx.is_closed()
    }
}

/// Reports whether a kill failed because the child is already gone.
///
/// `killpg` reports `ESRCH` when the target group no longer exists. Current
/// toolchains classify that number as `ErrorKind::Uncategorized` rather than
/// `NotFound`, so the raw OS error is matched explicitly on Unix. A missing
/// child is the terminal state termination asks for, not a fault.
///
/// `ErrorKind::NotFound` is deliberately not treated as absence. On Windows
/// that kind is how `Command::status` reports that the `taskkill` executable
/// itself could not be found, before anything signalled the child; reading it
/// as "already gone" would consume the killer, abort the helper tasks and
/// report a termination that never happened. Each backend that can prove the
/// target absent reports that proof itself, as the Windows `kill_process`
/// does after a failed `taskkill` and as the Unix `ESRCH` check does here.
fn is_already_gone(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ESRCH)
    }
    #[cfg(not(unix))]
    {
        let _ = error;
        false
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Use the async-drop pattern so cleanup can block on async waits
        // (e.g. waiting for the OS to reap the child) without blocking the
        // caller's thread. This mirrors testcontainers-rs's approach for
        // async resource cleanup from synchronous Drop.
        //
        // We must take ownership of the inner values here because the async
        // block needs to own everything it captures.
        let killer = self.killer.lock().ok().and_then(|mut g| g.take());
        let mut reader_handle = self.reader_handle.lock().ok().and_then(|mut g| g.take());
        let reader_abort_handles = self
            .reader_abort_handles
            .lock()
            .ok()
            .map(|mut g| g.drain(..).collect::<Vec<_>>());
        let mut writer_handle = self.writer_handle.lock().ok().and_then(|mut g| g.take());
        let mut wait_handle = self.wait_handle.lock().ok().and_then(|mut g| g.take());

        async_drop(move || async move {
            if let Some(mut killer) = killer {
                let _ = killer.kill();
            }
            if let Some(handle) = reader_handle.take() {
                handle.abort();
            }
            if let Some(handle) = writer_handle.take() {
                handle.abort();
            }
            if let Some(handle) = wait_handle.take() {
                handle.abort();
            }
            if let Some(handles) = reader_abort_handles {
                for handle in handles {
                    handle.abort();
                }
            }
        });
    }
}

/// Return value from spawn helpers (PTY or pipe).
///
/// Bundles the process handle with receivers for output and exit notification.
#[derive(Debug)]
pub struct SpawnedProcess {
    /// Handle for interacting with the process.
    pub session: ProcessHandle,
    /// Receiver for stdout/stderr output chunks.
    pub output_rx: broadcast::Receiver<Bytes>,
    /// Bounded, lossless receiver for consumers that must spool complete
    /// output. Unlike `output_rx`, this channel applies backpressure to the
    /// child-process readers instead of dropping lagged chunks.
    pub reliable_output_rx: mpsc::Receiver<Bytes>,
    /// Whether the producer is connected to `reliable_output_rx`.
    pub(crate) reliable_output_enabled: bool,
    /// Receiver for exit code (receives once when process exits).
    pub exit_rx: oneshot::Receiver<i32>,
}

impl SpawnedProcess {
    /// Convenience method to wait for the process to exit and collect output.
    ///
    /// Returns (collected_output, exit_code).
    pub async fn wait_with_output(self, timeout_ms: u64) -> (Vec<u8>, i32) {
        if self.reliable_output_enabled {
            collect_reliable_output_until_exit(self.reliable_output_rx, self.exit_rx, timeout_ms).await
        } else {
            collect_output_until_exit(self.output_rx, self.exit_rx, timeout_ms).await
        }
    }
}

/// Collect all output from the bounded process stream until exit or timeout.
async fn collect_reliable_output_until_exit(
    mut output_rx: mpsc::Receiver<Bytes>,
    exit_rx: oneshot::Receiver<i32>,
    timeout_ms: u64,
) -> (Vec<u8>, i32) {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    tokio::pin!(exit_rx);

    loop {
        tokio::select! {
            chunk = output_rx.recv() => {
                if let Some(chunk) = chunk {
                    collected.extend_from_slice(&chunk);
                } else {
                    return (collected, exit_rx.await.unwrap_or(-1));
                }
            }
            res = &mut exit_rx => {
                let code = res.unwrap_or(-1);
                // A descendant may inherit stdout/stderr after the direct
                // child exits. Keep the lossless path bounded just like the
                // compatibility broadcast path instead of waiting forever
                // for an inherited pipe descriptor to close.
                let quiet = tokio::time::Duration::from_millis(POST_EXIT_DRAIN_QUIET_MS);
                let max_deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(POST_EXIT_DRAIN_MAX_MS);
                while tokio::time::Instant::now() < max_deadline {
                    match tokio::time::timeout(quiet, output_rx.recv()).await {
                        Ok(Some(chunk)) => collected.extend_from_slice(&chunk),
                        Ok(None) | Err(_) => break,
                    }
                }
                return (collected, code);
            }
            _ = tokio::time::sleep_until(deadline) => {
                return (collected, -1);
            }
        }
    }
}

/// Collect output from a process until it exits or times out.
///
/// This is useful for tests and simple use cases where you want all output.
pub async fn collect_output_until_exit(
    mut output_rx: broadcast::Receiver<Bytes>,
    exit_rx: oneshot::Receiver<i32>,
    timeout_ms: u64,
) -> (Vec<u8>, i32) {
    let mut collected = Vec::new();
    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_millis(timeout_ms);
    tokio::pin!(exit_rx);

    loop {
        tokio::select! {
            res = output_rx.recv() => {
                if let Ok(chunk) = res {
                    collected.extend_from_slice(&chunk);
                }
            }
            res = &mut exit_rx => {
                let code = res.unwrap_or(-1);
                // Drain remaining output briefly after exit
                let quiet = tokio::time::Duration::from_millis(POST_EXIT_DRAIN_QUIET_MS);
                let max_deadline = tokio::time::Instant::now()
                    + tokio::time::Duration::from_millis(POST_EXIT_DRAIN_MAX_MS);

                while tokio::time::Instant::now() < max_deadline {
                    match tokio::time::timeout(quiet, output_rx.recv()).await {
                        Ok(Ok(chunk)) => collected.extend_from_slice(&chunk),
                        Ok(Err(broadcast::error::RecvError::Lagged(count))) => {
                            eprintln!("[diapason] output stream lagged ({count} dropped)");
                            continue;
                        }
                        Ok(Err(broadcast::error::RecvError::Closed)) => break,
                        Err(_) => break, // Timeout - quiet period reached
                    }
                }
                return (collected, code);
            }
            _ = tokio::time::sleep_until(deadline) => {
                return (collected, -1);
            }
        }
    }
}

/// Backwards-compatible alias for ProcessHandle.
pub type ExecCommandSession = ProcessHandle;

/// Backwards-compatible alias for SpawnedProcess.
pub type SpawnedPty = SpawnedProcess;

#[cfg(test)]
mod tests;
