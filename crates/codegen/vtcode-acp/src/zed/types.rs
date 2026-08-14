use crate::acp;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::time::Instant;
use vtcode_core::config::types::ReasoningEffortLevel;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::hooks::LifecycleHookEngine;

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
    pub(crate) audit_status: vtcode_safety::audit_log::ToolAuditStatus,
}

/// Cancellation signal that can be checked synchronously or awaited.
#[derive(Clone, Default)]
pub(crate) struct SessionCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl SessionCancellation {
    pub(crate) fn cancel(&self) {
        self.cancelled.store(true, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }

    pub(crate) fn reset(&self) {
        self.cancelled.store(false, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.notify.notified();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
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
}

pub(crate) struct SessionData {
    pub(crate) session_id: acp::SessionId,
    pub(crate) thread: ThreadRuntimeHandle,
    pub(crate) archive: Option<vtcode_core::utils::session_archive::SessionArchive>,
    pub(crate) workspace_runtime: Option<Arc<super::agent::SessionWorkspaceRuntime>>,
    #[allow(dead_code, reason = "Intentional compatibility, platform, or test-only suppression.")]
    pub(crate) tool_notice_sent: AtomicBool,
    pub(crate) primary_agent: String,
    pub(crate) reasoning_effort: ReasoningEffortLevel,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) last_tool_call_at: Option<Instant>,
    pub(crate) auto_compact_suppressed: u8,
    pub(crate) lifecycle_hooks: Option<LifecycleHookEngine>,
    pub(crate) session_started: bool,
    pub(crate) session_ended: bool,
}

impl SessionHandle {
    pub(crate) fn workspace_runtime(&self) -> Option<Arc<super::agent::SessionWorkspaceRuntime>> {
        self.data.lock().ok().and_then(|data| data.workspace_runtime.clone())
    }

    pub(crate) fn lifecycle_hooks(&self) -> Option<LifecycleHookEngine> {
        self.data.lock().ok().and_then(|data| data.lifecycle_hooks.clone())
    }

    pub(crate) fn has_stop_hooks(&self) -> bool {
        self.lifecycle_hooks().is_some_and(|hooks| hooks.has_stop_hooks())
    }

    pub(crate) fn mark_session_started(&self) -> bool {
        self.data
            .lock()
            .map(|mut data| {
                if data.session_started {
                    false
                } else {
                    data.session_started = true;
                    true
                }
            })
            .unwrap_or(false)
    }

    pub(crate) fn mark_session_ended(&self) -> bool {
        self.data
            .lock()
            .map(|mut data| {
                if data.session_ended {
                    false
                } else {
                    data.session_ended = true;
                    true
                }
            })
            .unwrap_or(false)
    }

    pub(crate) async fn update_transcript_path(&self) {
        let (hooks, path) = self
            .data
            .lock()
            .ok()
            .map(|data| {
                (data.lifecycle_hooks.clone(), data.archive.as_ref().map(|archive| archive.path().to_path_buf()))
            })
            .unwrap_or((None, None));
        if let Some(hooks) = hooks {
            hooks.update_transcript_path(path).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::SessionCancellation;

    #[tokio::test]
    async fn cancellation_wakes_a_pending_operation() {
        let cancellation = SessionCancellation::default();
        let waiter = cancellation.clone();
        let task = tokio::spawn(async move { waiter.cancelled().await });

        tokio::task::yield_now().await;
        cancellation.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("cancellation waiter should wake")
            .expect("waiter task should finish");
    }

    #[tokio::test]
    async fn reset_allows_the_signal_to_be_reused_for_the_next_turn() {
        let cancellation = SessionCancellation::default();
        cancellation.cancel();
        cancellation.cancelled().await;
        cancellation.reset();

        assert!(
            tokio::time::timeout(Duration::from_millis(10), cancellation.cancelled())
                .await
                .is_err(),
            "reset signal must wait for a fresh cancellation"
        );
    }
}
