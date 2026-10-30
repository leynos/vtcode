//! Bounded, payload-free observations of eligible automatic compaction passes.
//!
//! Metrics cover this automatic orchestration entry point and its terminal
//! result. Attempts use `unknown` before strategy selection. Terminal results
//! use the selected branch, overridden by the actual mode on success.

use std::time::Instant;

use hashbrown::HashMap;

use super::auto::AutoCompactionOutcome;
use crate::exec::events::CompactionMode;
use crate::llm::provider::{LLMError, Message};

/// Metric names are closed here rather than supplied by runtime inputs.
#[derive(Clone, Copy, Debug)]
enum Metric {
    Attempt,
    Success,
    NoChange,
    Failure,
    ProtectedBudgetRefusal,
    Duration,
}

impl Metric {
    /// Keep automatic-compaction event names stable across providers and sessions.
    const fn name(self) -> &'static str {
        match self {
            Self::Attempt => "vtcode.compaction.auto_attempt_total",
            Self::Success => "vtcode.compaction.auto_success_total",
            Self::NoChange => "vtcode.compaction.auto_no_change_total",
            Self::Failure => "vtcode.compaction.auto_failure_total",
            Self::ProtectedBudgetRefusal => "vtcode.compaction.auto_protected_budget_refusal_total",
            Self::Duration => "vtcode.compaction.auto_duration_ms",
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Outcome {
    Attempt,
    Success,
    NoChange,
    Failure,
}

impl Outcome {
    /// Terminal labels distinguish skipped work from failures without payload text.
    const fn as_str(self) -> &'static str {
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
enum FailureCategory {
    None,
    ProtectedUserBudget,
    Provider,
    Storage,
    Operation,
}

impl FailureCategory {
    /// Recognize wrapped typed causes without interpreting raw error messages.
    fn from_error(error: &anyhow::Error) -> Self {
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

    /// Export the finite failure vocabulary used by metrics and boundary logs.
    const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProtectedUserBudget => "protected_user_budget",
            Self::Provider => "provider",
            Self::Storage => "storage",
            Self::Operation => "operation",
        }
    }
}

#[derive(Clone, Copy)]
struct MetricContext {
    mode: CompactionMode,
    outcome: Outcome,
    error_category: FailureCategory,
}

struct MetricObservation {
    metric: Metric,
    context: MetricContext,
}

impl MetricObservation {
    /// Bind each observation to closed metric and context values.
    fn new(metric: Metric, mode: CompactionMode, outcome: Outcome, error_category: FailureCategory) -> Self {
        Self {
            metric,
            context: MetricContext { mode, outcome, error_category },
        }
    }
}

impl MetricContext {
    /// Only finite mode, outcome, and error-category values enter tags.
    fn tags(self) -> HashMap<String, String> {
        HashMap::from([
            ("mode".to_owned(), self.mode.as_str().to_owned()),
            ("outcome".to_owned(), self.outcome.as_str().to_owned()),
            ("error_category".to_owned(), self.error_category.as_str().to_owned()),
        ])
    }
}

/// Invocation-owned sink; tests do not alter the process perf recorder.
pub(super) enum AutoCompactionMetricSink {
    Process,
    #[cfg(test)]
    Test(std::sync::Arc<std::sync::Mutex<Vec<crate::telemetry::TelemetryEvent>>>),
}

impl AutoCompactionMetricSink {
    /// Share one projection between the perf pipeline and invocation-owned tests.
    fn record(&self, metric: Metric, value: f64, context: MetricContext) {
        let name = metric.name();
        let tags = context.tags();
        match self {
            Self::Process => crate::telemetry::perf::record_value(name, value, tags),
            #[cfg(test)]
            Self::Test(recorder) => {
                let mut event = crate::telemetry::TelemetryEvent::new(name, value);
                event.tags = tags;
                recorder.lock().expect("test metric recorder lock").push(event);
            }
        }
    }
}

/// Measures one admitted automatic attempt through its publication boundary.
pub(super) struct AutoCompactionTelemetry<'a> {
    sink: &'a AutoCompactionMetricSink,
    tokens_before: usize,
    started: Instant,
}

impl<'a> AutoCompactionTelemetry<'a> {
    /// Begin after enablement, trigger, and suppression checks have passed.
    pub(super) fn start(sink: &'a AutoCompactionMetricSink, history: &[Message]) -> Self {
        sink.record(
            Metric::Attempt,
            1.0,
            MetricContext {
                mode: CompactionMode::Unknown,
                outcome: Outcome::Attempt,
                error_category: FailureCategory::None,
            },
        );
        Self {
            sink,
            tokens_before: history.iter().map(Message::estimate_tokens).sum(),
            started: Instant::now(),
        }
    }

    /// Project the terminal result without exposing input or error payloads.
    pub(super) fn finish(
        self,
        result: &anyhow::Result<Option<AutoCompactionOutcome>>,
        history: &[Message],
        selected_mode: CompactionMode,
    ) {
        let MetricObservation { metric, context } = Self::result_observation(result, selected_mode);
        self.sink.record(metric, 1.0, context);
        self.sink
            .record(Metric::Duration, self.started.elapsed().as_secs_f64() * 1000.0, context);
        if context.error_category == FailureCategory::ProtectedUserBudget {
            self.sink.record(Metric::ProtectedBudgetRefusal, 1.0, context);
        }
        if result.is_err() {
            let tokens_after: usize = history.iter().map(Message::estimate_tokens).sum();
            tracing::warn!(
                mode = context.mode.as_str(),
                error_category = context.error_category.as_str(),
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
            Ok(Some(compacted)) => {
                MetricObservation::new(Metric::Success, compacted.mode, Outcome::Success, FailureCategory::None)
            }
            Ok(None) => {
                MetricObservation::new(Metric::NoChange, selected_mode, Outcome::NoChange, FailureCategory::None)
            }
            Err(error) => MetricObservation::new(
                Metric::Failure,
                selected_mode,
                Outcome::Failure,
                FailureCategory::from_error(error),
            ),
        }
    }
}
