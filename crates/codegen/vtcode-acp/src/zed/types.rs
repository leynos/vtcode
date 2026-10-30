//! Defines the shared Zed ACP session, tool, and run-state types used across adapter modules.
use crate::acp;
use anyhow::Context;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, Notify};
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
        if self.closing.load(Ordering::Acquire) {
            return None;
        }
        let _previous_active_prompts = self.active_prompts.fetch_add(1, Ordering::AcqRel);
        if self.closing.load(Ordering::Acquire) {
            self.finish_prompt();
            return None;
        }
        Some(SessionPromptGuard { activity: self.clone() })
    }

    fn begin_close(&self) {
        self.closing.store(true, Ordering::Release);
    }

    async fn wait_for_prompts(&self) {
        loop {
            let notified = self.prompts_idle.notified();
            tokio::pin!(notified);
            let _was_newly_enabled = notified.as_mut().enable();
            if self.active_prompts.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }

    fn finish_prompt(&self) {
        if self.active_prompts.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.prompts_idle.notify_waiters();
        }
    }

    fn admitted_prompt_count(&self) -> usize {
        self.active_prompts.load(Ordering::Acquire)
    }
}

impl SessionPromptGuard {
    pub(crate) fn is_closing(&self) -> bool {
        self.activity.closing.load(Ordering::Acquire)
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
    /// Serializes prompt turns for this session. ACP permits clients to send a
    /// new `session/prompt` while another turn is active; turns run one at a
    /// time instead of failing the turn guard or racing the cancellation flag.
    /// Waiters acquire the gate in the order their spawned tasks reach it, so
    /// queued steers have no stricter arrival-order guarantee. The mutex is
    /// held for the whole turn, so the guard must only be awaited inside a
    /// spawned task.
    pub(crate) prompt_gate: Arc<AsyncMutex<()>>,
}

impl SessionHandle {
    /// Acquires the per-session turn gate, waiting for any earlier prompt to
    /// finish. Callers must already have distinguished an unknown session;
    /// awaiting here is only safe inside a spawned task.
    pub(crate) async fn acquire_prompt_gate(&self) -> tokio::sync::OwnedMutexGuard<()> {
        Arc::clone(&self.prompt_gate).lock_owned().await
    }

    /// Prompts admitted for this session that have not finished yet, including
    /// the caller. Sampled before waiting on the gate, this is the queue depth
    /// a steering prompt joins; it is observational only and never rejects a
    /// steer.
    pub(crate) fn admitted_prompt_count(&self) -> usize {
        self.activity.admitted_prompt_count()
    }
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
}

pub(crate) struct SessionData {
    pub(in crate::zed) subagent_controller: Option<Arc<vtcode_core::subagents::SubagentController>>,
    pub(crate) session_id: acp::SessionId,
    pub(crate) thread: ThreadRuntimeHandle,
    pub(crate) archive: Option<vtcode_core::utils::session_archive::SessionArchive>,
    pub(in crate::zed) workspace_runtime: Option<Arc<super::agent::SessionWorkspaceRuntime>>,
    pub(super) workspace_lease: Option<super::agent::SessionWorkspaceLease>,
    pub(crate) primary_agent: String,
    pub(crate) reasoning_effort: ReasoningEffortLevel,
    pub(crate) provider: String,
    pub(crate) model: String,
    pub(crate) last_tool_call_at: Option<Instant>,
    pub(crate) auto_compact_suppressed: u8,
    pub(crate) lifecycle_hooks: Option<LifecycleHookEngine>,
    pub(crate) session_started: bool,
    pub(crate) session_ended: bool,
    pub(crate) task_lifecycle_forwarder: Option<tokio::task::JoinHandle<()>>,
    pub(crate) task_lifecycle_registration: Option<vtcode_core::subagents::SubagentProgressObserverRegistration>,
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
        if let Some(controller) = controller {
            controller.signal_shutdown().await;
        }
        drop(runtime);
        if let Some(lease) = lease {
            lease.release().await;
        }
        Ok(())
    }

    pub(crate) async fn stop_task_lifecycle_forwarder(&self) -> anyhow::Result<()> {
        let (forwarder, registration) = {
            let mut data = self
                .data
                .lock()
                .map_err(|error| lock_poison_error("stopping ACP task lifecycle forwarding", error))?;
            (data.task_lifecycle_forwarder.take(), data.task_lifecycle_registration.take())
        };
        drop(registration);
        if let Some(forwarder) = forwarder {
            forwarder.abort();
            match forwarder.await {
                Ok(()) => {}
                Err(error) if error.is_cancelled() => {}
                Err(error) => return Err(error).context("waiting for ACP task lifecycle forwarder to stop"),
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{SessionActivity, SessionCancellation, SessionPromptGuard};

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
        assert!(cancellation.reset());

        assert!(
            tokio::time::timeout(Duration::from_millis(10), cancellation.cancelled())
                .await
                .is_err(),
            "reset signal must wait for a fresh cancellation"
        );
    }

    #[test]
    fn close_cancellation_cannot_be_reset_for_another_prompt() {
        let cancellation = SessionCancellation::default();
        cancellation.close();

        assert!(!cancellation.reset());
        assert!(cancellation.is_cancelled());
        assert!(cancellation.is_closed());
    }

    /// Reference model for [`SessionCancellation`]'s two sticky flags.
    ///
    /// `cancel` and `close` are idempotent setters, and `reset` clears only
    /// `cancelled` — and only while the session has not been closed. The
    /// generated sequences are sequential; they do not prove anything about
    /// concurrent interleavings.
    #[derive(Debug, Default)]
    struct CancellationModel {
        cancelled: bool,
        closed: bool,
    }

    impl CancellationModel {
        fn cancel(&mut self) {
            self.cancelled = true;
        }

        fn close(&mut self) {
            self.cancelled = true;
            self.closed = true;
        }

        fn reset(&mut self) -> bool {
            if self.closed {
                return false;
            }
            self.cancelled = false;
            true
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum CancellationOp {
        Cancel,
        Close,
        Reset,
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 128,
            max_shrink_iters: 256,
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn cancellation_flags_match_the_reference_model(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::Just(CancellationOp::Cancel),
                    proptest::prelude::Just(CancellationOp::Close),
                    proptest::prelude::Just(CancellationOp::Reset),
                ],
                0..12,
            ),
        ) {
            let cancellation = SessionCancellation::default();
            let mut model = CancellationModel::default();

            for op in ops {
                match op {
                    CancellationOp::Cancel => {
                        cancellation.cancel();
                        model.cancel();
                    }
                    CancellationOp::Close => {
                        cancellation.close();
                        model.close();
                    }
                    CancellationOp::Reset => {
                        proptest::prop_assert_eq!(
                            cancellation.reset(),
                            model.reset(),
                            "reset must report whether the signal was revived"
                        );
                    }
                }

                proptest::prop_assert_eq!(cancellation.is_cancelled(), model.cancelled);
                proptest::prop_assert_eq!(cancellation.is_closed(), model.closed);
            }

            // Closing is terminal: no later operation may revive the session.
            if model.closed {
                proptest::prop_assert!(cancellation.is_closed());
                proptest::prop_assert!(!cancellation.reset());
                proptest::prop_assert!(cancellation.is_cancelled());
            }
        }
    }

    /// Reference model for [`SessionActivity`] and [`SessionPromptGuard`].
    ///
    /// A guard exists exactly while its prompt is in flight: starting one
    /// increments the in-flight count, and dropping the guard decrements it
    /// exactly once. Closing is sticky — once `begin_close` runs no further
    /// prompt may start, and guards taken before the close stay valid so the
    /// close can wait for them. `wait_for_prompts` completes if and only if the
    /// in-flight count is zero.
    ///
    /// The generated sequences are sequential. They pin the bookkeeping a close
    /// depends on; they say nothing about concurrent interleavings.
    #[derive(Debug, Default)]
    struct ActivityModel {
        in_flight: usize,
        closing: bool,
    }

    impl ActivityModel {
        /// Mirrors the double-check in `try_start_prompt`: a close that lands
        /// between the two checks must not leave a prompt counted.
        fn try_start_prompt(&mut self) -> bool {
            if self.closing {
                return false;
            }
            self.in_flight += 1;
            true
        }

        fn drop_guard(&mut self) {
            assert!(self.in_flight > 0, "reference model underflow");
            self.in_flight -= 1;
        }

        fn begin_close(&mut self) {
            self.closing = true;
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum ActivityOp {
        StartPrompt,
        DropGuard,
        Close,
        WaitForPrompts,
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig {
            cases: 64,
            max_shrink_iters: 256,
            ..proptest::prelude::ProptestConfig::default()
        })]

        #[test]
        fn session_activity_matches_the_reference_model(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::Just(ActivityOp::StartPrompt),
                    proptest::prelude::Just(ActivityOp::DropGuard),
                    proptest::prelude::Just(ActivityOp::Close),
                    proptest::prelude::Just(ActivityOp::WaitForPrompts),
                ],
                0..16,
            ),
        ) {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("current-thread runtime");
            let _guard = runtime.enter();

            let activity = SessionActivity::default();
            let mut model = ActivityModel::default();
            let mut guards: Vec<SessionPromptGuard> = Vec::new();

            for op in ops {
                match op {
                    ActivityOp::StartPrompt => {
                        let guard = activity.try_start_prompt();
                        let model_grants = model.try_start_prompt();
                        proptest::prop_assert_eq!(
                            guard.is_some(),
                            model_grants,
                            "prompt admission must match the model"
                        );
                        match guard {
                            Some(guard) => {
                                proptest::prop_assert_eq!(
                                    guard.is_closing(),
                                    model.closing,
                                    "a guard must report the close state it was taken under"
                                );
                                guards.push(guard);
                            }
                            None => {
                                proptest::prop_assert!(
                                    model.closing,
                                    "a refused prompt must be explained by a pending close"
                                );
                            }
                        }
                    }
                    ActivityOp::DropGuard => {
                        if let Some(guard) = guards.pop() {
                            drop(guard);
                            model.drop_guard();
                        }
                    }
                    ActivityOp::Close => {
                        activity.begin_close();
                        model.begin_close();
                    }
                    ActivityOp::WaitForPrompts => {
                        let idle = runtime.block_on(tokio::time::timeout(
                            Duration::from_millis(50),
                            activity.wait_for_prompts(),
                        ));
                        proptest::prop_assert_eq!(
                            idle.is_ok(),
                            model.in_flight == 0,
                            "waiting for prompts must complete exactly when none are in flight"
                        );
                    }
                }

                // A pending close is sticky for the rest of the sequence. The
                // refused branch of `try_start_prompt` does not mutate the
                // count, so this observation cannot perturb the model.
                if model.closing {
                    proptest::prop_assert!(
                        activity.try_start_prompt().is_none(),
                        "a closing session must refuse every later prompt"
                    );
                }
            }

            // Closing is sticky and does not invalidate guards taken earlier:
            // an outstanding prompt must keep the session non-idle until it is
            // dropped, and then the wait must complete.
            activity.begin_close();
            let outstanding = guards.len();
            proptest::prop_assert_eq!(outstanding, model.in_flight);
            if outstanding > 0 {
                proptest::prop_assert!(
                    runtime
                        .block_on(tokio::time::timeout(
                            Duration::from_millis(50),
                            activity.wait_for_prompts()
                        ))
                        .is_err(),
                    "an outstanding prompt must keep the close waiting"
                );
            }
            guards.clear();
            runtime
                .block_on(tokio::time::timeout(
                    Duration::from_millis(500),
                    activity.wait_for_prompts(),
                ))
                .expect("dropping every guard must let the close finish");
        }
    }
}
