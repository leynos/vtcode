//! Bounded prompt scheduling and completion metrics.
//!
//! Prompt observations share [`ProviderMetricSink::emit`] with the provider
//! retry vocabulary, but own their fixed names, outcome labels, and tests.

use std::time::Duration;

use hashbrown::HashMap;

use super::provider_telemetry::{ProviderMetricSink, ProviderMode};

const PROMPT_GATE_QUEUE_DEPTH: &str = "vtcode.acp.prompt_gate_queue_depth";
const PROMPT_GATE_WAIT_MS: &str = "vtcode.acp.prompt_gate_wait_ms";
const PROMPT_OUTCOME_TOTAL: &str = "vtcode.acp.prompt_outcome_total";

/// Fixed outcome vocabulary for a finished ACP prompt turn.
///
/// Only the sanitized failure reasons ACP produces itself get their own
/// series; every other error is grouped as `failed`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PromptOutcome {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
    EmptyResponse,
    ToolExecutionFailed,
    Failed,
}

impl PromptOutcome {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::EndTurn => "end_turn",
            Self::MaxTokens => "max_tokens",
            Self::MaxTurnRequests => "max_turn_requests",
            Self::Refusal => "refusal",
            Self::Cancelled => "cancelled",
            Self::EmptyResponse => "empty_response",
            Self::ToolExecutionFailed => "tool_execution_failed",
            Self::Failed => "failed",
        }
    }
}

/// Bounded observations for prompt scheduling and completion.
///
/// These carry no request data: the only labels are the prompt mode and the
/// closed [`PromptOutcome`] vocabulary.
#[derive(Clone, Copy, Debug)]
pub(super) enum PromptMetricObservation {
    GateQueueDepth { depth: usize },
    GateWait { duration: Duration },
    Outcome { mode: ProviderMode, outcome: PromptOutcome },
}

impl PromptMetricObservation {
    /// Projects a prompt observation into its fixed metric name, value, and
    /// labels.
    fn into_record(self) -> (&'static str, f64, HashMap<String, String>) {
        match self {
            Self::GateQueueDepth { depth } => {
                let depth = u32::try_from(depth).map_or(f64::from(u32::MAX), f64::from);
                (PROMPT_GATE_QUEUE_DEPTH, depth, HashMap::new())
            }
            Self::GateWait { duration } => (PROMPT_GATE_WAIT_MS, duration.as_secs_f64() * 1000.0, HashMap::new()),
            Self::Outcome { mode, outcome } => {
                let mut tags = HashMap::new();
                let _previous_mode = tags.insert("mode".to_owned(), mode.as_str().to_owned());
                let _previous_outcome = tags.insert("outcome".to_owned(), outcome.as_str().to_owned());
                (PROMPT_OUTCOME_TOTAL, 1.0, tags)
            }
        }
    }
}

impl ProviderMetricSink {
    /// Records a prompt scheduling or completion observation.
    pub(super) async fn record_prompt(&self, observation: PromptMetricObservation) {
        let (name, value, tags) = observation.into_record();
        self.emit(name, value, tags).await;
    }
}

#[cfg(test)]
mod tests {
    use super::{PromptMetricObservation, PromptOutcome};
    use crate::zed::agent::provider_telemetry::ProviderMode;
    use std::time::Duration;

    #[test]
    fn prompt_metric_descriptors_use_exact_finite_vocabulary() {
        let records: Vec<_> = [
            PromptMetricObservation::GateQueueDepth { depth: 2 },
            PromptMetricObservation::GateWait { duration: Duration::from_millis(250) },
            PromptMetricObservation::Outcome {
                mode: ProviderMode::Stream,
                outcome: PromptOutcome::Cancelled,
            },
            PromptMetricObservation::Outcome {
                mode: ProviderMode::PreInit,
                outcome: PromptOutcome::Refusal,
            },
        ]
        .into_iter()
        .map(PromptMetricObservation::into_record)
        .collect();

        assert_eq!(records[0].0, "vtcode.acp.prompt_gate_queue_depth");
        assert_eq!(records[0].1.to_bits(), 2.0_f64.to_bits());
        assert!(records[0].2.is_empty());
        assert_eq!(records[1].0, "vtcode.acp.prompt_gate_wait_ms");
        assert_eq!(records[1].1.to_bits(), 250.0_f64.to_bits());
        assert!(records[1].2.is_empty());
        assert_eq!(records[2].0, "vtcode.acp.prompt_outcome_total");
        assert_eq!(records[2].1.to_bits(), 1.0_f64.to_bits());
        assert_eq!(records[2].2.len(), 2);
        assert_eq!(records[2].2.get("mode").map(String::as_str), Some("stream"));
        assert_eq!(records[2].2.get("outcome").map(String::as_str), Some("cancelled"));
        assert_eq!(records[3].2.get("mode").map(String::as_str), Some("pre_init"));
        assert_eq!(records[3].2.get("outcome").map(String::as_str), Some("refusal"));
    }

    #[test]
    fn prompt_outcomes_use_a_fixed_vocabulary() {
        let labels: Vec<_> = [
            PromptOutcome::EndTurn,
            PromptOutcome::MaxTokens,
            PromptOutcome::MaxTurnRequests,
            PromptOutcome::Refusal,
            PromptOutcome::Cancelled,
            PromptOutcome::EmptyResponse,
            PromptOutcome::ToolExecutionFailed,
            PromptOutcome::Failed,
        ]
        .into_iter()
        .map(PromptOutcome::as_str)
        .collect();

        assert_eq!(
            labels,
            vec![
                "end_turn",
                "max_tokens",
                "max_turn_requests",
                "refusal",
                "cancelled",
                "empty_response",
                "tool_execution_failed",
                "failed",
            ]
        );
    }

    #[test]
    fn prompt_modes_use_a_fixed_vocabulary() {
        assert_eq!(
            [
                ProviderMode::Buffered.as_str(),
                ProviderMode::Stream.as_str(),
                ProviderMode::PreInit.as_str()
            ],
            ["buffered", "stream", "pre_init"]
        );
    }
}
