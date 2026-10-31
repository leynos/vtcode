//! Test-only synchronization helpers shared by ACP agent test modules.

use crate::acp;
use crate::zed::agent::ZedAgent;

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
