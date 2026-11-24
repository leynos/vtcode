//! Defines the shared Zed ACP session, tool, and run-state types used across adapter modules.
use crate::acp;
use anyhow::Context;
use diapason_core::config::types::ReasoningEffortLevel;
use diapason_core::core::threads::ThreadRuntimeHandle;
use diapason_core::hooks::LifecycleHookEngine;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) enum ToolRuntime {
    Enabled,
    Disabled,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RunTerminalMode {
    Terminal,
    Pty,
}

pub(crate) struct ToolCallResult {
    pub(crate) tool_call_id: String,
    pub(crate) llm_response: String,
    pub(crate) audit_status: diapason_safety::audit_log::ToolAuditStatus,
    pub(crate) audit_arguments: Vec<u8>,
}

/// Cancellation signal that can be checked synchronously or awaited.
#[derive(Clone, Default)]
pub(crate) struct SessionCancellation {
    state: Arc<AtomicU8>,
    notify: Arc<Notify>,
}

impl SessionCancellation {
    pub(crate) fn cancel(&self) {
        let _previous_state = self.state.fetch_or(CANCELLED, Ordering::AcqRel);
        self.notify.notify_waiters();
    }

    pub(crate) fn close(&self) {
        let _previous_state = self.state.fetch_or(CANCELLED | CLOSED, Ordering::AcqRel);
        self.notify.notify_waiters();
    }

    pub(crate) fn reset(&self) -> bool {
        loop {
            let state = self.state.load(Ordering::Acquire);
            if state & CLOSED != 0 {
                return false;
            }
            if self
                .state
                .compare_exchange(state, state & !CANCELLED, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.state.load(Ordering::Acquire) & CANCELLED != 0
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) & CLOSED != 0
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            let _was_newly_enabled = notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }
}

const CANCELLED: u8 = 1;
const CLOSED: u8 = 2;

#[derive(Clone, Default)]
pub(crate) struct SessionActivity {
    closing: Arc<AtomicBool>,
    active_prompts: Arc<AtomicUsize>,
    prompts_idle: Arc<Notify>,
}

pub(crate) struct SessionPromptGuard {
    activity: SessionActivity,
}

impl SessionActivity {
    fn try_start_prompt(&self) -> Option<SessionPromptGuard> {
        // Paired with `begin_close` and `wait_for_prompts` as a Dekker-style
        // handshake: seq-cst ordering makes the closing store and the active
        // prompt counter observe each other, so a close cannot both miss a
        // starting prompt and let that prompt miss the close.
        if self.closing.load(Ordering::SeqCst) {
            return None;
        }
        let _previous_active_prompts = self.active_prompts.fetch_add(1, Ordering::SeqCst);
        if self.closing.load(Ordering::SeqCst) {
            self.finish_prompt();
            return None;
        }
        Some(SessionPromptGuard { activity: self.clone() })
    }

    fn begin_close(&self) {
        self.closing.store(true, Ordering::SeqCst);
    }

    /// Reports whether the session has begun closing.
    ///
    /// Closing is sticky, and it stays observable on the shared activity so a
    /// clone of the handle can still tell that the session is going away.
    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::SeqCst)
    }

    async fn wait_for_prompts(&self) {
        loop {
            let notified = self.prompts_idle.notified();
            tokio::pin!(notified);
            let _was_newly_enabled = notified.as_mut().enable();
            if self.active_prompts.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn finish_prompt(&self) {
        if self.active_prompts.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.prompts_idle.notify_waiters();
        }
    }
}

impl SessionPromptGuard {
    pub(crate) fn is_closing(&self) -> bool {
        self.activity.is_closing()
    }
}

impl Drop for SessionPromptGuard {
    fn drop(&mut self) {
        self.activity.finish_prompt();
    }
}

/// Per-session handle. `Arc`-shared so the agent can hand a clone to a
/// spawned task that drives the prompt loop. The data is `Send + Sync`
/// (backed by thread-safe synchronization primitives) so it can travel across the
/// `LocalSet`-less task boundary that SACP's `cx.spawn` enforces.
#[derive(Clone)]
pub(crate) struct SessionHandle {
    pub(crate) data: Arc<Mutex<SessionData>>,
    pub(crate) cancellation: SessionCancellation,
    pub(crate) activity: SessionActivity,
}

impl SessionHandle {
    pub(crate) fn try_start_prompt(&self) -> Option<SessionPromptGuard> {
        self.activity.try_start_prompt()
    }

    pub(crate) fn begin_close(&self) {
        self.activity.begin_close();
        self.cancellation.close();
    }

    pub(crate) async fn wait_for_prompts(&self) {
        self.activity.wait_for_prompts().await;
    }

    /// Reports whether the session has begun closing.
    ///
    /// This flag marks the start of the close sequence, not its completion: the
    /// session's workspace runtime is still held and is released later by the
    /// cleanup path (`finish_closed_session` releases the lease after waiting
    /// for in-flight prompts). The flag is the authority a management request
    /// uses to tell that it lost the race: `resolve_open_session_controller`
    /// checks it before and after resolving a controller and reports
    /// `unknown_session` when it is set. It is not evidence that the runtime
    /// has already been released, so a caller that has another use for the
    /// runtime must not read it as one; a management request must.
    pub(crate) fn is_closing(&self) -> bool {
        self.activity.is_closing()
    }
}

pub(crate) struct SessionData {
    pub(in crate::zed) subagent_controller: Option<Arc<diapason_core::subagents::SubagentController>>,
    pub(crate) session_id: acp::SessionId,
    pub(crate) thread: ThreadRuntimeHandle,
    pub(crate) archive: Option<diapason_core::utils::session_archive::SessionArchive>,
    pub(in crate::zed) workspace_runtime: Option<Arc<super::agent::SessionWorkspaceRuntime>>,
    pub(super) workspace_lease: Option<super::agent::SessionWorkspaceLease>,
    pub(crate) primary_agent: String,
    pub(crate) reasoning_effort: ReasoningEffortLevel,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) last_tool_call_at: Option<Instant>,
    /// Call ids whose write-ahead evidence the last required checkpoint made
    /// durable. Best-effort checkpoints must not compact them away.
    pub(crate) write_ahead_call_ids: Vec<String>,
    pub(crate) auto_compact_suppressed: u8,
    pub(crate) lifecycle_hooks: Option<LifecycleHookEngine>,
    pub(crate) session_started: bool,
    pub(crate) session_ended: bool,
    pub(crate) task_lifecycle_forwarder: Option<tokio::task::JoinHandle<()>>,
    /// The one-shot publisher of the initial task snapshots for this
    /// forwarder. It writes into the forwarder's queue, so it must be aborted
    /// with the forwarder; otherwise it outlives the receiver and keeps
    /// enqueueing into a queue that nothing drains.
    pub(crate) task_lifecycle_snapshot_publisher: Option<tokio::task::JoinHandle<()>>,
    pub(crate) task_lifecycle_registration: Option<diapason_core::subagents::SubagentProgressObserverRegistration>,
}

#[derive(Clone, Copy)]
enum SessionLifecycleBoundary {
    Started,
    Ended,
}

impl SessionLifecycleBoundary {
    const fn lock_context(self) -> &'static str {
        match self {
            Self::Started => "marking ACP session started",
            Self::Ended => "marking ACP session ended",
        }
    }
}

enum SessionLifecycleTransition {
    Started,
    Ended(String),
}

impl SessionData {
    fn mark_lifecycle_boundary(&mut self, boundary: SessionLifecycleBoundary) -> Option<SessionLifecycleTransition> {
        match boundary {
            SessionLifecycleBoundary::Started if self.session_started => None,
            SessionLifecycleBoundary::Started => {
                self.session_started = true;
                Some(SessionLifecycleTransition::Started)
            }
            SessionLifecycleBoundary::Ended if self.session_ended => None,
            SessionLifecycleBoundary::Ended => {
                self.session_ended = true;
                Some(SessionLifecycleTransition::Ended(self.session_id.to_string()))
            }
        }
    }
}

pub(crate) fn lock_poison_error(context: &str, error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!("{context}: {error}")
}

impl Drop for SessionData {
    fn drop(&mut self) {
        if let Some(snapshot_publisher) = self.task_lifecycle_snapshot_publisher.take() {
            snapshot_publisher.abort();
        }
        if let Some(forwarder) = self.task_lifecycle_forwarder.take() {
            forwarder.abort();
        }
    }
}

impl SessionHandle {
    pub(in crate::zed) fn workspace_runtime(
        &self,
    ) -> anyhow::Result<Option<Arc<super::agent::SessionWorkspaceRuntime>>> {
        self.data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session workspace runtime", error))
            .map(|data| data.workspace_runtime.clone())
    }

    pub(crate) async fn release_workspace_lease(&self) -> anyhow::Result<()> {
        let (lease, runtime, controller) = {
            let mut data = self
                .data
                .lock()
                .map_err(|error| lock_poison_error("releasing ACP session workspace lease", error))?;
            (data.workspace_lease.take(), data.workspace_runtime.take(), data.subagent_controller.take())
        };
        // The cleanup runs on a task of its own, in the order it always ran.
        // The lease has already left the session, so a cancelled caller would
        // otherwise drop the last lease without ever starting the runtime
        // shutdown it asks for: the lease's own `Drop` can only publish a
        // pending shutdown because it cannot await. Spawning before the first
        // await makes cancellation detach the task instead of stranding the
        // lease.
        let cleanup = tokio::spawn(async move {
            if let Some(controller) = controller {
                controller.signal_shutdown().await;
            }
            drop(runtime);
            if let Some(lease) = lease {
                lease.release().await;
            }
        });
        cleanup.await.context("waiting for ACP workspace lease release")?;
        Ok(())
    }

    pub(crate) async fn stop_task_lifecycle_forwarder(&self) -> anyhow::Result<()> {
        let (forwarder, snapshot_publisher, registration) = {
            let mut data = self
                .data
                .lock()
                .map_err(|error| lock_poison_error("stopping ACP task lifecycle forwarding", error))?;
            (
                data.task_lifecycle_forwarder.take(),
                data.task_lifecycle_snapshot_publisher.take(),
                data.task_lifecycle_registration.take(),
            )
        };
        drop(registration);
        stop_task_lifecycle_tasks(forwarder, snapshot_publisher).await
    }

    pub(crate) fn lifecycle_hooks(&self) -> anyhow::Result<Option<LifecycleHookEngine>> {
        self.data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session lifecycle hooks", error))
            .map(|data| data.lifecycle_hooks.clone())
    }

    pub(crate) fn has_stop_hooks(&self) -> anyhow::Result<bool> {
        Ok(self.lifecycle_hooks()?.is_some_and(|hooks| hooks.has_stop_hooks()))
    }

    pub(crate) fn mark_session_started(&self) -> anyhow::Result<bool> {
        Ok(self.mark_lifecycle_boundary(SessionLifecycleBoundary::Started)?.is_some())
    }

    pub(crate) fn mark_session_ended(&self) -> anyhow::Result<Option<String>> {
        Ok(match self.mark_lifecycle_boundary(SessionLifecycleBoundary::Ended)? {
            Some(SessionLifecycleTransition::Ended(session_id)) => Some(session_id),
            _ => None,
        })
    }

    fn mark_lifecycle_boundary(
        &self,
        boundary: SessionLifecycleBoundary,
    ) -> anyhow::Result<Option<SessionLifecycleTransition>> {
        self.data
            .lock()
            .map_err(|error| lock_poison_error(boundary.lock_context(), error))
            .map(|mut data| data.mark_lifecycle_boundary(boundary))
    }

    pub(crate) async fn update_transcript_path(&self) -> anyhow::Result<()> {
        let (hooks, path) = self
            .data
            .lock()
            .map_err(|error| lock_poison_error("reading ACP session state to update transcript path", error))
            .map(|data| {
                (data.lifecycle_hooks.clone(), data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
            })?;
        if let Some(hooks) = hooks {
            hooks.update_transcript_path(path).await;
        }
        Ok(())
    }
}

/// Stops the lifecycle forwarder and its initial-snapshot publisher.
///
/// Aborting the publisher first keeps it from enqueueing while the receiver is
/// awaited to completion, and both are aborted before either is awaited. A
/// non-cancellation join error from either task is saved and returned only
/// after both have been awaited: returning at the publisher's panic would leave
/// the forwarder detached and still running. The publisher's error is returned
/// first because it was captured first.
async fn stop_task_lifecycle_tasks(
    forwarder: Option<tokio::task::JoinHandle<()>>,
    snapshot_publisher: Option<tokio::task::JoinHandle<()>>,
) -> anyhow::Result<()> {
    if let Some(snapshot_publisher) = &snapshot_publisher {
        snapshot_publisher.abort();
    }
    if let Some(forwarder) = &forwarder {
        forwarder.abort();
    }
    let publisher_error = match snapshot_publisher {
        Some(snapshot_publisher) => match snapshot_publisher.await {
            Ok(()) => None,
            Err(error) if error.is_cancelled() => None,
            Err(error) => {
                Some(anyhow::Error::new(error).context("waiting for ACP task lifecycle snapshot publisher to stop"))
            }
        },
        None => None,
    };
    let forwarder_error = match forwarder {
        Some(forwarder) => match forwarder.await {
            Ok(()) => None,
            Err(error) if error.is_cancelled() => None,
            Err(error) => Some(anyhow::Error::new(error).context("waiting for ACP task lifecycle forwarder to stop")),
        },
        None => None,
    };
    if let Some(error) = publisher_error {
        return Err(error);
    }
    if let Some(error) = forwarder_error {
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
