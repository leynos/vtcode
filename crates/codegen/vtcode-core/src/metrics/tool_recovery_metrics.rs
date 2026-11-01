//! Bounded counters for ACP interrupted tool-call recovery.
//!
//! Every dimension is a fixed event variant. Session identifiers, tool names,
//! call identifiers, and tool output never become a label, so an export cannot
//! leak the content or names of the tools a session invoked.

use serde::{Deserialize, Serialize};

/// One bounded ACP tool-recovery event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRecoveryEvent {
    /// An incomplete placeholder was staged before its tool call executed.
    StagedCall,
    /// A staged placeholder was replaced by its terminal tool result.
    ReplacedCall,
    /// An archived call without a terminal result received an explicit
    /// incomplete result instead of being replayed.
    InterruptedCall,
    /// A uniquely attributable late result was moved beside its request batch.
    RelocatedResult,
    /// An archive's repaired history was persisted and checkpointed.
    RepairedArchive,
    /// An archive was refused because its terminal results were ambiguous.
    AmbiguousHistory,
    /// A prompt was rejected because another turn already owned the session.
    ConcurrentPromptRejection,
    /// `write_stdin` resolved an identifier with no live exec session.
    StaleWriteStdinRequest,
    /// A session checkpoint failed.
    CheckpointFailure,
}

impl ToolRecoveryEvent {
    /// Every event, in the order rendered for Prometheus.
    pub const ALL: [Self; 9] = [
        Self::StagedCall,
        Self::ReplacedCall,
        Self::InterruptedCall,
        Self::RelocatedResult,
        Self::RepairedArchive,
        Self::AmbiguousHistory,
        Self::ConcurrentPromptRejection,
        Self::StaleWriteStdinRequest,
        Self::CheckpointFailure,
    ];

    /// Stable Prometheus label and log field for this event.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::StagedCall => "staged_call",
            Self::ReplacedCall => "replaced_call",
            Self::InterruptedCall => "interrupted_call",
            Self::RelocatedResult => "relocated_result",
            Self::RepairedArchive => "repaired_archive",
            Self::AmbiguousHistory => "ambiguous_history",
            Self::ConcurrentPromptRejection => "concurrent_prompt_rejection",
            Self::StaleWriteStdinRequest => "stale_write_stdin_request",
            Self::CheckpointFailure => "checkpoint_failure",
        }
    }
}

/// Bounded ACP tool-recovery counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolRecoveryMetrics {
    /// Incomplete placeholders staged before execution.
    pub staged_calls: u64,
    /// Placeholders replaced by terminal results.
    pub replaced_calls: u64,
    /// Archived calls repaired with an explicit incomplete result.
    pub interrupted_calls: u64,
    /// Late results relocated beside their request batch.
    pub relocated_results: u64,
    /// Repaired archives that were checkpointed before registration.
    pub repaired_archives: u64,
    /// Archives refused because tool-result attribution was ambiguous.
    pub ambiguous_histories: u64,
    /// Prompts rejected because another turn owned the session.
    pub concurrent_prompt_rejections: u64,
    /// `write_stdin` requests for a session that no longer exists.
    pub stale_write_stdin_requests: u64,
    /// Failed session checkpoints.
    pub checkpoint_failures: u64,
}

impl ToolRecoveryMetrics {
    /// Count one event.
    pub fn record(&mut self, event: ToolRecoveryEvent) {
        self.record_many(event, 1);
    }

    /// Count `count` occurrences of one event.
    ///
    /// Recovery works in batches, so a single repair may resolve many calls.
    pub fn record_many(&mut self, event: ToolRecoveryEvent, count: usize) {
        let count = u64::try_from(count).unwrap_or(u64::MAX);
        let counter = match event {
            ToolRecoveryEvent::StagedCall => &mut self.staged_calls,
            ToolRecoveryEvent::ReplacedCall => &mut self.replaced_calls,
            ToolRecoveryEvent::InterruptedCall => &mut self.interrupted_calls,
            ToolRecoveryEvent::RelocatedResult => &mut self.relocated_results,
            ToolRecoveryEvent::RepairedArchive => &mut self.repaired_archives,
            ToolRecoveryEvent::AmbiguousHistory => &mut self.ambiguous_histories,
            ToolRecoveryEvent::ConcurrentPromptRejection => &mut self.concurrent_prompt_rejections,
            ToolRecoveryEvent::StaleWriteStdinRequest => &mut self.stale_write_stdin_requests,
            ToolRecoveryEvent::CheckpointFailure => &mut self.checkpoint_failures,
        };
        *counter = counter.saturating_add(count);
    }

    /// Current value of one event counter.
    pub fn count(&self, event: ToolRecoveryEvent) -> u64 {
        match event {
            ToolRecoveryEvent::StagedCall => self.staged_calls,
            ToolRecoveryEvent::ReplacedCall => self.replaced_calls,
            ToolRecoveryEvent::InterruptedCall => self.interrupted_calls,
            ToolRecoveryEvent::RelocatedResult => self.relocated_results,
            ToolRecoveryEvent::RepairedArchive => self.repaired_archives,
            ToolRecoveryEvent::AmbiguousHistory => self.ambiguous_histories,
            ToolRecoveryEvent::ConcurrentPromptRejection => self.concurrent_prompt_rejections,
            ToolRecoveryEvent::StaleWriteStdinRequest => self.stale_write_stdin_requests,
            ToolRecoveryEvent::CheckpointFailure => self.checkpoint_failures,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ToolRecoveryEvent, ToolRecoveryMetrics};
    use crate::metrics::MetricsCollector;

    #[test]
    fn tool_recovery_events_are_bounded_in_summary_json_and_prometheus() {
        let collector = MetricsCollector::new();
        collector.record_tool_recovery(ToolRecoveryEvent::StagedCall);
        collector.record_tool_recovery_events(ToolRecoveryEvent::InterruptedCall, 3);
        collector.record_tool_recovery(ToolRecoveryEvent::AmbiguousHistory);

        let summary = collector.get_summary();
        assert_eq!(summary.tool_recovery.staged_calls, 1);
        assert_eq!(summary.tool_recovery.interrupted_calls, 3);
        assert_eq!(summary.tool_recovery.ambiguous_histories, 1);

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["tool_recovery"]["staged_calls"], 1);
        assert_eq!(json["tool_recovery"]["interrupted_calls"], 3);
        // Labels are fixed variants, so no call content can reach the export.
        let rendered = json.to_string();
        assert!(!rendered.contains("tool_call_id"));

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains(r#"vtcode_tool_recovery_total{event="staged_call"} 1"#));
        assert!(prometheus.contains(r#"vtcode_tool_recovery_total{event="interrupted_call"} 3"#));
    }

    #[test]
    fn counts_saturate_instead_of_wrapping() {
        let mut metrics = ToolRecoveryMetrics {
            staged_calls: u64::MAX,
            ..ToolRecoveryMetrics::default()
        };
        metrics.record(ToolRecoveryEvent::StagedCall);
        assert_eq!(metrics.count(ToolRecoveryEvent::StagedCall), u64::MAX);
    }
}
