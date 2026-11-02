//! Runtime adapter projecting automatic-compaction observations into perf metrics.
//!
//! The compaction domain emits [`AutoCompactionEvent`] values through its
//! observation port. This adapter owns the process metric vocabulary and the
//! opt-in perf pipeline dependency, keeping the domain independent of process
//! telemetry.

#[cfg(any(test, feature = "telemetry-test-support"))]
use std::sync::{Arc, Mutex};

use hashbrown::HashMap;

use crate::compaction::{AutoCompactionEvent, AutoCompactionObserver, FailureCategory, Outcome};
use crate::exec::events::CompactionMode;
#[cfg(any(test, feature = "telemetry-test-support"))]
use crate::telemetry::TelemetryEvent;
use crate::telemetry::perf;

/// Production observer backed by the opt-in process perf pipeline.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessAutoCompactionObserver;

/// Shared production observer for call sites that do not own one.
pub static PROCESS_AUTO_COMPACTION_OBSERVER: ProcessAutoCompactionObserver = ProcessAutoCompactionObserver;

impl AutoCompactionObserver for ProcessAutoCompactionObserver {
    fn observe(&self, event: AutoCompactionEvent) {
        let (name, value, tags) = project(event);
        perf::record_value(name, value, tags);
    }
}

/// Invocation-owned recorder for tests and downstream test support.
#[cfg(any(test, feature = "telemetry-test-support"))]
#[derive(Clone, Debug, Default)]
pub struct RecordingAutoCompactionObserver {
    events: Arc<Mutex<Vec<TelemetryEvent>>>,
}

#[cfg(any(test, feature = "telemetry-test-support"))]
impl RecordingAutoCompactionObserver {
    /// Create an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a snapshot of the recorded events.
    pub fn events(&self) -> Vec<TelemetryEvent> {
        self.events.lock().expect("auto-compaction metric recorder lock").clone()
    }
}

#[cfg(any(test, feature = "telemetry-test-support"))]
impl AutoCompactionObserver for RecordingAutoCompactionObserver {
    fn observe(&self, event: AutoCompactionEvent) {
        let (name, value, tags) = project(event);
        let mut recorded = TelemetryEvent::new(name, value);
        recorded.tags = tags;
        self.events.lock().expect("auto-compaction metric recorder lock").push(recorded);
    }
}

/// Project one domain event into the closed process metric vocabulary.
fn project(event: AutoCompactionEvent) -> (&'static str, f64, HashMap<String, String>) {
    match event {
        AutoCompactionEvent::Attempt => (
            "vtcode.compaction.auto_attempt_total",
            1.0,
            tags(CompactionMode::Unknown, Outcome::Attempt, FailureCategory::None),
        ),
        AutoCompactionEvent::Success { mode } => {
            ("vtcode.compaction.auto_success_total", 1.0, tags(mode, Outcome::Success, FailureCategory::None))
        }
        AutoCompactionEvent::NoChange { mode } => {
            ("vtcode.compaction.auto_no_change_total", 1.0, tags(mode, Outcome::NoChange, FailureCategory::None))
        }
        AutoCompactionEvent::Failure { mode, category } => {
            ("vtcode.compaction.auto_failure_total", 1.0, tags(mode, Outcome::Failure, category))
        }
        AutoCompactionEvent::ProtectedBudgetRefusal { mode, category } => {
            ("vtcode.compaction.auto_protected_budget_refusal_total", 1.0, tags(mode, Outcome::Failure, category))
        }
        AutoCompactionEvent::Duration { mode, outcome, category, millis } => {
            ("vtcode.compaction.auto_duration_ms", millis, tags(mode, outcome, category))
        }
    }
}

/// Build the finite label set shared by every automatic-compaction metric.
fn tags(mode: CompactionMode, outcome: Outcome, category: FailureCategory) -> HashMap<String, String> {
    HashMap::from([
        ("mode".to_owned(), mode.as_str().to_owned()),
        ("outcome".to_owned(), outcome.as_str().to_owned()),
        ("error_category".to_owned(), category.as_str().to_owned()),
    ])
}
