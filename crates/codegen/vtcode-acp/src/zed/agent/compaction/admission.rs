//! Observes ACP admission decisions and publishes only admitted history.
//!
//! The core engine has already returned successfully at this boundary. These
//! metrics describe ACP acceptance/refusal, never generic engine failure. Metric
//! names and both label values form a closed vocabulary; no request identifiers,
//! provider/model names, diagnostics, prompts or token budgets enter the sink.

use super::admit_compacted_history;
use anyhow::Result;
use hashbrown::HashMap;
use std::time::{Duration, Instant};
use tracing::warn;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::provider::Message;

const ATTEMPT_TOTAL: &str = "vtcode.acp.compaction_admission_attempt_total";
const RESULT_TOTAL: &str = "vtcode.acp.compaction_admission_result_total";
const DURATION_MS: &str = "vtcode.acp.compaction_admission_duration_ms";

pub(super) struct AdmissionCandidate {
    pub(super) thread: ThreadRuntimeHandle,
    pub(super) history: Vec<Message>,
    pub(super) prompt_tokens: usize,
    pub(super) original_history_tokens: usize,
    pub(super) configured_threshold: Option<usize>,
    pub(super) admission_budget: Option<usize>,
    pub(super) changed: bool,
}

impl AdmissionCandidate {
    fn admit(&self) -> Result<usize> {
        if self.changed {
            return admit_compacted_history(
                &self.history,
                self.prompt_tokens,
                self.original_history_tokens,
                self.admission_budget,
            );
        }
        warn!(
            prompt_tokens = self.prompt_tokens,
            configured_threshold = ?self.configured_threshold,
            admission_budget = ?self.admission_budget,
            "ACP context pressure crossed its admission budget but compaction made no change"
        );
        if self.admission_budget.is_some_and(|budget| self.prompt_tokens >= budget) {
            anyhow::bail!(
                "ACP prompt uses an estimated {} tokens, exceeds its safe provider admission budget, and automatic compaction made no change",
                self.prompt_tokens,
            );
        }
        Ok(self.prompt_tokens)
    }
}

/// The same boundary serves runtime and recorder-owned tests. Admission remains
/// pure; observation is awaited before successful history publication, and the
/// caller checkpoints only after this boundary returns an accepted candidate.
pub(super) async fn admit_and_publish(candidate: AdmissionCandidate, sink: &AdmissionMetricSink) -> Result<usize> {
    let started_at = Instant::now();
    let result = candidate.admit();
    let outcome = AdmissionOutcome::from_decision(candidate.changed, result.is_ok());
    sink.record(outcome, started_at.elapsed()).await;
    let prompt_tokens = result?;
    if candidate.changed {
        candidate.thread.replace_messages(candidate.history);
    }
    Ok(prompt_tokens)
}

#[derive(Clone, Copy)]
enum AdmissionOutcome {
    Accepted,
    NoChange,
    CandidateOverBudget,
    UnchangedOverBudget,
}

impl AdmissionOutcome {
    const fn from_decision(changed: bool, admitted: bool) -> Self {
        match (changed, admitted) {
            (true, true) => Self::Accepted,
            (false, true) => Self::NoChange,
            (true, false) => Self::CandidateOverBudget,
            (false, false) => Self::UnchangedOverBudget,
        }
    }

    const fn outcome(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::NoChange => "no_change",
            Self::CandidateOverBudget | Self::UnchangedOverBudget => "refused",
        }
    }

    const fn reason(self) -> &'static str {
        match self {
            Self::Accepted => "within_budget",
            Self::NoChange => "unchanged_within_budget",
            Self::CandidateOverBudget => "candidate_over_budget",
            Self::UnchangedOverBudget => "unchanged_over_budget",
        }
    }

    fn tags(self) -> HashMap<String, String> {
        HashMap::from([
            ("outcome".to_owned(), self.outcome().to_owned()),
            ("reason".to_owned(), self.reason().to_owned()),
        ])
    }
}

/// Production uses the existing opt-in perf pipeline; tests own their recorder
/// and await each event without installing global state or a capture hook.
pub(super) enum AdmissionMetricSink {
    Process,
    #[cfg(test)]
    Test(std::sync::Arc<vtcode_core::telemetry::perf::TestPerfRecorder>),
}

impl AdmissionMetricSink {
    async fn record(&self, outcome: AdmissionOutcome, elapsed: Duration) {
        self.record_value(ATTEMPT_TOTAL, 1.0, HashMap::new()).await;
        self.record_value(RESULT_TOTAL, 1.0, outcome.tags()).await;
        self.record_value(DURATION_MS, elapsed.as_secs_f64() * 1_000.0, outcome.tags())
            .await;
    }

    async fn record_value(&self, name: &'static str, value: f64, tags: HashMap<String, String>) {
        match self {
            Self::Process => vtcode_core::telemetry::perf::record_value(name, value, tags),
            #[cfg(test)]
            Self::Test(recorder) => recorder
                .record_value(name, value, tags)
                .await
                .expect("record ACP admission metric"),
        }
    }
}

#[cfg(test)]
mod tests;
