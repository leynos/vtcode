//! Test-only synchronization helpers shared by ACP agent test modules.

use crate::acp;
use crate::zed::agent::ZedAgent;
use std::sync::Mutex;
use tokio::sync::Notify;

/// Waits until `expected` prompts are acquiring the session's prompt gate.
///
/// A steer only becomes observable here after the agent admits the request and
/// its spawned task reaches the gate, so this proves the steer is queued before
/// a test releases or cancels the turn it waits for. Yielding, rather than
/// sleeping, keeps the ordering deterministic.
pub(crate) async fn wait_for_gate_waiters(agent: &ZedAgent, session_id: &acp::SessionId, expected: usize) {
    for _ in 0..10_000 {
        let waiters = agent
            .session_handle(session_id)
            .expect("look up ACP session")
            .expect("ACP session exists")
            .prompt_gate_waiter_count();
        if waiters >= expected {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the prompt gate never reported {expected} waiter(s)");
}

/// Waits until the agent has observed cancellation for the session.
///
/// Sending `session/cancel` only writes the frame; the agent records the
/// cancellation when it processes the notification. Tests that pause a turn
/// inside finalization must wait for that observation before releasing it,
/// otherwise the cancel could lose the race it is meant to win.
pub(crate) async fn wait_for_cancellation(agent: &ZedAgent, session_id: &acp::SessionId) {
    for _ in 0..10_000 {
        let cancelled = agent
            .session_handle(session_id)
            .expect("look up ACP session")
            .expect("ACP session exists")
            .cancellation
            .is_cancelled();
        if cancelled {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the session was never cancelled");
}

/// Test-only pause point inside a named session-checkpoint boundary.
///
/// `Notify::notify_one` stores a permit when no waiter is present, so arming
/// and releasing from the test without an interleaved await stays
/// deterministic.
#[derive(Default)]
pub(crate) struct CheckpointPause {
    target: Mutex<Option<&'static str>>,
    reached: Notify,
    release: Notify,
}

impl CheckpointPause {
    /// Arms the pause for the next checkpoint persisted at `boundary`.
    pub(crate) fn arm(&self, boundary: &'static str) {
        *self.target.lock().expect("checkpoint pause target") = Some(boundary);
    }

    /// Blocks inside an armed `boundary` until [`Self::release`] is called.
    pub(crate) async fn wait_at(&self, boundary: &'static str) {
        {
            let mut target = self.target.lock().expect("checkpoint pause target");
            if *target != Some(boundary) {
                return;
            }
            *target = None;
        }
        self.reached.notify_one();
        self.release.notified().await;
    }

    /// Waits until a turn is paused at the armed checkpoint boundary.
    pub(crate) async fn reached(&self) {
        self.reached.notified().await;
    }

    /// Releases a turn paused at the armed checkpoint boundary.
    pub(crate) fn release(&self) {
        self.release.notify_one();
    }
}
