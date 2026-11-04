//! RL optimization loop: adaptive action selection.
//!
//! Diapason couples its modular runtime with a data/evaluation strategy so the
//! harness can prefer low-latency, high-success actions (e.g. edge vs cloud
//! executors) over time. This is the real implementation behind the
//! `docs/ARCHITECTURE.md` "RL Optimization Loop" section:
//!
//! - Command/sandbox outcomes are captured through the existing `bash_runner`
//!   and PTY subsystems (no extra instrumentation).
//! - Each outcome becomes a [`crate::llm::rl::RewardSignal`] (success + latency + cost).
//! - Signals accumulate in a rolling [`crate::llm::rl::RewardLedger`] keyed by action id.
//! - [`crate::llm::rl::RlEngine::select`] picks the next action via UCB / epsilon-greedy
//!   bandit logic, or an actor-critic stand-in, driven by `[optimization].rl`.
//!
//! The module is decomposed into independently testable chunks:
//! [`crate::llm::rl::signal`] (reward math + strategy), [`crate::llm::rl::ledger`] (rolling statistics),
//! [`crate::llm::rl::engine`] (selection policy), and [`crate::llm::rl::eval`] (eval-report bridge).

pub mod engine;
pub mod eval;
pub mod ledger;
pub mod signal;

pub use engine::{Action, PolicyContext, RlEngine, RlSnapshot};
pub use eval::reward_from_eval_report;
pub use ledger::RewardLedger;
pub use signal::{RewardSignal, RlStrategy};
