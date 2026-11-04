use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use diapason_ui::tui::app::InlineHandle;

/// Runs `launch` on a blocking thread via `spawn_blocking`, giving the
/// external application exclusive access to the terminal while it runs.
///
/// The `OwnedTuiSuspensionGuard` is **moved into** the `spawn_blocking`
/// closure, so the TUI is restored only when the blocking closure actually
/// exits (i.e. the editor/scrollback returns) — **not** when the async caller
/// is cancelled. This prevents terminal input contention between the TUI event
/// stream and a still-running editor process after task cancellation.
///
/// Requires `F: Send + 'static` and `T: Send + 'static` because the
/// closure is sent to the blocking thread pool.  Callers must prepare
/// owned data before calling this function.
pub(crate) async fn run_blocking_with_event_loop_suspended<T, F>(
    handle: &InlineHandle,
    suspend_tui: bool,
    launch: F,
) -> Result<T>
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: Send + 'static,
{
    // Create the owned guard on the async side — this suspends the TUI event
    // stream immediately.
    let guard = OwnedTuiSuspensionGuard::new(handle.clone(), suspend_tui);
    if guard.active {
        // Give the background EventStream task time to stop before clearing
        // the input queue and yielding the terminal to the editor.
        tokio::time::sleep(EVENT_STREAM_STOP_DELAY).await;
        handle.clear_input_queue();
    }

    // Move the guard INTO the blocking closure. If the async caller is
    // cancelled while awaiting, the JoinHandle is dropped but the blocking
    // task continues to completion. The guard drops inside the closure when
    // the editor exits, restoring the TUI at the right time.
    tokio::task::spawn_blocking(move || {
        let _guard = guard;
        launch()
    })
    .await
    .map_err(|e| anyhow::anyhow!("suspended blocking operation panicked: {e}"))?
}

pub(crate) async fn run_with_event_loop_suspended_async<T, F, Fut>(
    handle: &InlineHandle,
    suspend_tui: bool,
    launch: F,
) -> Result<T>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    // The async path uses a borrowed guard on the async stack. If the task is
    // cancelled, the future is dropped and the guard restores the TUI
    // immediately — which is correct here because the async launch future is
    // genuinely cancelled (no blocking process continues).
    let guard = TuiSuspensionGuard::new(handle, suspend_tui);
    if guard.is_active() {
        tokio::time::sleep(EVENT_STREAM_STOP_DELAY).await;
        handle.clear_input_queue();
    }

    let result = launch().await;
    drop(guard);
    result
}

const EVENT_STREAM_STOP_DELAY: Duration = Duration::from_millis(150);

/// Borrowed guard for the async path — lives on the async stack, restores
/// the TUI on `Drop` (including cancellation-driven drop).
struct TuiSuspensionGuard<'a> {
    handle: &'a InlineHandle,
    active: bool,
}

impl<'a> TuiSuspensionGuard<'a> {
    fn new(handle: &'a InlineHandle, active: bool) -> Self {
        if active {
            // Fully stop the background EventStream task so terminal editors
            // have exclusive access to stdin.
            handle.stop_event_stream();
        }
        Self { handle, active }
    }

    fn is_active(&self) -> bool {
        self.active
    }
}

impl Drop for TuiSuspensionGuard<'_> {
    fn drop(&mut self) {
        if self.active {
            self.handle.clear_input_queue();
            self.handle.resume_event_loop();
            self.handle.start_event_stream();
        }
    }
}

/// Owned guard for the blocking path — cloned `InlineHandle` so it can be
/// moved into a `spawn_blocking` closure. The TUI is restored only when the
/// closure exits (editor returns), even if the async caller was cancelled.
struct OwnedTuiSuspensionGuard {
    handle: InlineHandle,
    active: bool,
}

impl OwnedTuiSuspensionGuard {
    fn new(handle: InlineHandle, active: bool) -> Self {
        if active {
            handle.stop_event_stream();
        }
        Self { handle, active }
    }
}

impl Drop for OwnedTuiSuspensionGuard {
    fn drop(&mut self) {
        if self.active {
            self.handle.clear_input_queue();
            self.handle.resume_event_loop();
            self.handle.start_event_stream();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use diapason_ui::tui::app::{InlineCommand, InlineHandle};
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn cancellation_restores_suspended_event_loop() {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(command_tx);
        let (started_tx, started_rx) = oneshot::channel();
        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            run_with_event_loop_suspended_async(&task_handle, true, || async move {
                let _ = started_tx.send(());
                std::future::pending::<Result<()>>().await
            })
            .await
        });

        started_rx.await.expect("launch closure should start");
        task.abort();
        task.await.expect_err("suspended operation should be cancelled");

        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::StopEventStream)));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::ClearInputQueue)));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::ClearInputQueue)));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::ResumeEventLoop)));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::StartEventStream)));
    }

    /// Cancellation of `run_blocking_with_event_loop_suspended` must **not**
    /// restore the TUI event loop until the blocking closure actually exits.
    /// The `OwnedTuiSuspensionGuard` is moved into the `spawn_blocking`
    /// closure, so even after the async caller is aborted, the TUI stays
    /// suspended while the editor/scrollback process is still running. The
    /// guard restores the TUI only when the closure returns.
    #[tokio::test]
    async fn cancellation_does_not_restore_tui_until_blocking_exits() {
        let (command_tx, mut command_rx) = mpsc::unbounded_channel();
        let handle = InlineHandle::new_for_tests(command_tx);
        let (started_tx, started_rx) = oneshot::channel::<()>();
        let started_tx = std::sync::Mutex::new(Some(started_tx));

        let task_handle = handle.clone();
        let task = tokio::spawn(async move {
            run_blocking_with_event_loop_suspended(&task_handle, true, move || {
                // Signal that the blocking closure has started, then sleep
                // long enough for the test to abort the task and verify the
                // TUI is NOT restored yet.
                if let Some(tx) = started_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
                std::thread::sleep(Duration::from_millis(300));
                Ok(())
            })
            .await
        });

        started_rx.await.expect("blocking closure should start");
        task.abort();
        task.await.expect_err("suspended blocking operation should be cancelled");

        // Before the blocking closure exits, only the suspend commands should
        // have been sent (StopEventStream + ClearInputQueue from the pre-spawn
        // phase). The restore commands must NOT have been sent yet.
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::StopEventStream)));
        assert!(matches!(command_rx.try_recv(), Ok(InlineCommand::ClearInputQueue)));
        // No restore commands yet — the TUI is still suspended.
        assert!(
            command_rx.try_recv().is_err(),
            "TUI must NOT be restored while the blocking closure is still running"
        );

        // Wait for the blocking closure to exit (guard's Drop fires inside
        // the closure, sending restore commands). Use a timeout to avoid
        // hanging if the guard logic is broken.
        let restore_deadline = Duration::from_millis(2000);
        assert!(
            matches!(
                tokio::time::timeout(restore_deadline, command_rx.recv()).await,
                Ok(Some(InlineCommand::ClearInputQueue))
            ),
            "guard Drop must send ClearInputQueue after closure exits"
        );
        assert!(matches!(
            tokio::time::timeout(restore_deadline, command_rx.recv()).await,
            Ok(Some(InlineCommand::ResumeEventLoop))
        ));
        assert!(matches!(
            tokio::time::timeout(restore_deadline, command_rx.recv()).await,
            Ok(Some(InlineCommand::StartEventStream))
        ));
    }
}
