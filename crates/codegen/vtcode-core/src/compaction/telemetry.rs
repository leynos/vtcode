//! Bounded, payload-free observations of eligible automatic compaction passes.
//!
//! The compaction domain owns the observation vocabulary and the timing
//! boundary. Runtime adapters implement [`AutoCompactionObserver`] to project
//! those events into their own telemetry pipeline, and supply a
//! [`MonotonicClock`] so duration measurement is deterministic in tests.
//!
//! Metrics cover this automatic orchestration entry point and its terminal
//! result. Attempts use `unknown` before strategy selection. Terminal results
//! use the selected branch, overridden by the actual mode on success.

use std::time::Instant;

use super::auto::AutoCompactionOutcome;
use crate::exec::events::CompactionMode;
use crate::llm::provider::{LLMError, Message};

/// Narrow monotonic clock used by telemetry boundaries.
pub trait MonotonicClock: Send + Sync {
    /// Return the current monotonic instant.
    fn now(&self) -> Instant;
}

/// Production clock backed by the process monotonic timer.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl MonotonicClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Shared production clock for call sites that do not own one.
pub static SYSTEM_CLOCK: SystemClock = SystemClock;

/// Deterministic clock for tests and downstream test support.
#[cfg(any(test, feature = "telemetry-test-support"))]
#[derive(Debug)]
pub struct TestClock {
    now: std::sync::Mutex<Instant>,
}

#[cfg(any(test, feature = "telemetry-test-support"))]
impl TestClock {
    /// Start at the current process instant.
    pub fn new() -> Self {
        Self { now: std::sync::Mutex::new(Instant::now()) }
    }

    /// Advance the clock by `duration`.
    pub fn advance(&self, duration: std::time::Duration) {
        let mut now = self.now.lock().expect("test clock lock");
        *now = now.checked_add(duration).unwrap_or(*now);
    }
}

#[cfg(any(test, feature = "telemetry-test-support"))]
impl Default for TestClock {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(any(test, feature = "telemetry-test-support"))]
impl MonotonicClock for TestClock {
    fn now(&self) -> Instant {
        *self.now.lock().expect("test clock lock")
    }
}

/// Terminal outcome label for automatic-compaction observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// An eligible pass started.
    Attempt,
    /// The pass produced a compacted history.
    Success,
    /// The pass completed without changing history.
    NoChange,
    /// The pass failed.
    Failure,
}

impl Outcome {
    /// Stable label used by runtime metric adapters.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Attempt => "attempt",
            Self::Success => "success",
            Self::NoChange => "no_change",
            Self::Failure => "failure",
        }
    }
}

/// Classify typed causes; provider text never determines metric labels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureCategory {
    /// No failure occurred.
    None,
    /// Protected user instructions exceed the model budget.
    ProtectedUserBudget,
    /// The provider rejected or failed the request.
    Provider,
    /// A storage or filesystem operation failed.
    Storage,
    /// Any other operation failed.
    Operation,
}

impl FailureCategory {
    /// Recognize wrapped typed causes without interpreting raw error messages.
    pub fn from_error(error: &anyhow::Error) -> Self {
        if super::is_protected_user_budget_error(error) {
            Self::ProtectedUserBudget
        } else if error.is::<LLMError>() {
            Self::Provider
        } else if error.is::<std::io::Error>() {
            Self::Storage
        } else {
            Self::Operation
        }
    }

    /// Stable label used by runtime metric adapters.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProtectedUserBudget => "protected_user_budget",
            Self::Provider => "provider",
            Self::Storage => "storage",
            Self::Operation => "operation",
        }
    }
}

/// Bounded, payload-free observation of one automatic-compaction pass.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AutoCompactionEvent {
    /// An eligible pass started.
    Attempt,
    /// The pass produced a compacted history.
    Success {
        /// Mode that produced the compacted history.
        mode: CompactionMode,
    },
    /// The pass completed without changing history.
    NoChange {
        /// Mode selected for the pass.
        mode: CompactionMode,
    },
    /// The pass failed.
    Failure {
        /// Mode selected for the pass.
        mode: CompactionMode,
        /// Typed failure classification.
        category: FailureCategory,
    },
    /// The pass refused because protected user instructions exceed the budget.
    ProtectedBudgetRefusal {
        /// Mode selected for the pass.
        mode: CompactionMode,
        /// Typed failure classification.
        category: FailureCategory,
    },
    /// The pass duration in milliseconds.
    Duration {
        /// Mode selected for the pass.
        mode: CompactionMode,
        /// Terminal outcome of the pass.
        outcome: Outcome,
        /// Typed failure classification.
        category: FailureCategory,
        /// Elapsed milliseconds measured by the injected clock.
        millis: f64,
    },
}

/// Domain-shaped observation port for automatic compaction.
pub trait AutoCompactionObserver: Send + Sync {
    /// Record one bounded, payload-free observation.
    fn observe(&self, event: AutoCompactionEvent);
}

/// Measures one admitted automatic attempt through its publication boundary.
pub(super) struct AutoCompactionTelemetry<'a> {
    observer: &'a dyn AutoCompactionObserver,
    clock: &'a dyn MonotonicClock,
    tokens_before: usize,
    started: Instant,
}

impl<'a> AutoCompactionTelemetry<'a> {
    /// Begin after enablement, trigger, and suppression checks have passed.
    pub(super) fn start(
        observer: &'a dyn AutoCompactionObserver,
        clock: &'a dyn MonotonicClock,
        history: &[Message],
    ) -> Self {
        observer.observe(AutoCompactionEvent::Attempt);
        Self {
            observer,
            clock,
            tokens_before: history.iter().map(Message::estimate_tokens).sum(),
            started: clock.now(),
        }
    }

    /// Project the terminal result without exposing input or error payloads.
    pub(super) fn finish(
        self,
        result: &anyhow::Result<Option<AutoCompactionOutcome>>,
        history: &[Message],
        selected_mode: CompactionMode,
    ) {
        let observation = Self::result_observation(result, selected_mode);
        self.observer.observe(observation.event);
        let millis = self.clock.now().saturating_duration_since(self.started).as_secs_f64() * 1000.0;
        self.observer.observe(AutoCompactionEvent::Duration {
            mode: observation.mode,
            outcome: observation.outcome,
            category: observation.error_category,
            millis,
        });
        if observation.error_category == FailureCategory::ProtectedUserBudget {
            self.observer.observe(AutoCompactionEvent::ProtectedBudgetRefusal {
                mode: observation.mode,
                category: observation.error_category,
            });
        }
        if result.is_err() {
            let tokens_after: usize = history.iter().map(Message::estimate_tokens).sum();
            tracing::warn!(
                mode = observation.mode.as_str(),
                error_category = observation.error_category.as_str(),
                tokens_before = self.tokens_before,
                tokens_after,
                "Automatic compaction failed"
            );
        }
    }

    /// Successful results supply the actual mode after native/local fallback.
    fn result_observation(
        result: &anyhow::Result<Option<AutoCompactionOutcome>>,
        selected_mode: CompactionMode,
    ) -> MetricObservation {
        match result {
            Ok(Some(compacted)) => MetricObservation {
                event: AutoCompactionEvent::Success { mode: compacted.mode },
                mode: compacted.mode,
                outcome: Outcome::Success,
                error_category: FailureCategory::None,
            },
            Ok(None) => MetricObservation {
                event: AutoCompactionEvent::NoChange { mode: selected_mode },
                mode: selected_mode,
                outcome: Outcome::NoChange,
                error_category: FailureCategory::None,
            },
            Err(error) => {
                let category = FailureCategory::from_error(error);
                MetricObservation {
                    event: AutoCompactionEvent::Failure { mode: selected_mode, category },
                    mode: selected_mode,
                    outcome: Outcome::Failure,
                    error_category: category,
                }
            }
        }
    }
}

struct MetricObservation {
    event: AutoCompactionEvent,
    mode: CompactionMode,
    outcome: Outcome,
    error_category: FailureCategory,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::compaction::RecordingAutoCompactionObserver;

    #[test]
    fn duration_uses_the_injected_clock() {
        let clock = TestClock::new();
        let observer = RecordingAutoCompactionObserver::new();
        let history = vec![Message::user("task".into())];
        let telemetry = AutoCompactionTelemetry::start(&observer, &clock, &history);
        clock.advance(std::time::Duration::from_millis(250));
        let result: anyhow::Result<Option<AutoCompactionOutcome>> = Ok(None);

        telemetry.finish(&result, &history, CompactionMode::Local);

        let events = observer.events();
        let duration = events
            .iter()
            .find(|event| event.name == "vtcode.compaction.auto_duration_ms")
            .expect("duration metric");
        assert_eq!(duration.value.to_bits(), 250.0_f64.to_bits());
    }
}
