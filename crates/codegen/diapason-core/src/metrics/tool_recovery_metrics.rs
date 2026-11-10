//! Bounded counters for interrupted tool-call recovery.
//!
//! This vocabulary is transport-neutral: it names recovery states shared by
//! the core agent loop and any adapter, and it imports no protocol schema
//! types. Core records the events it originates (a stale `write_stdin`
//! continuation); an adapter records the events it observes (staging,
//! replacement, live interruption, archive repair). Every dimension is a fixed
//! event variant.
//! Session identifiers, tool names, call identifiers, and tool output never
//! become a label, so an export cannot leak the content or names of the tools
//! a session invoked.

use serde::{Deserialize, Serialize};

/// One bounded tool-recovery event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRecoveryEvent {
    /// An incomplete placeholder was staged before its tool call executed.
    StagedCall,
    /// A staged placeholder was replaced by its terminal tool result.
    ReplacedCall,
    /// A tool call was interrupted before its terminal result was recorded.
    ///
    /// Archive repair records one event per repaired call. A live adapter round
    /// records one event per call in the interrupted batch.
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
    /// A best-effort checkpoint was skipped to keep protected evidence durable.
    ///
    /// The write was intentionally not attempted because the compacted payload
    /// would have dropped write-ahead tool-call evidence. The previous archive
    /// remains intact, so this is a degraded checkpoint rather than a failure.
    CheckpointSkipped,
    /// A provider tool-call batch was rejected because its IDs were not unique.
    ///
    /// The batch is rejected before any placeholder is staged or any tool
    /// executes, so this counter records a refused round rather than a
    /// recovered one.
    DuplicateToolCallBatch,
}

impl ToolRecoveryEvent {
    /// Every event, in the order rendered for Prometheus.
    pub const ALL: [Self; 11] = [
        Self::StagedCall,
        Self::ReplacedCall,
        Self::InterruptedCall,
        Self::RelocatedResult,
        Self::RepairedArchive,
        Self::AmbiguousHistory,
        Self::ConcurrentPromptRejection,
        Self::StaleWriteStdinRequest,
        Self::CheckpointFailure,
        Self::CheckpointSkipped,
        Self::DuplicateToolCallBatch,
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
            Self::CheckpointSkipped => "checkpoint_skipped",
            Self::DuplicateToolCallBatch => "duplicate_tool_call_batch",
        }
    }
}

/// Bounded tool-recovery counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolRecoveryMetrics {
    /// Incomplete placeholders staged before execution.
    pub staged_calls: u64,
    /// Placeholders replaced by terminal results.
    pub replaced_calls: u64,
    /// Interrupted calls, counted per call.
    ///
    /// Archive repair and live interruption both increment this counter.
    /// It counts calls, not unique call identifiers, so a batch that is
    /// interrupted twice contributes twice.
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
    /// Best-effort checkpoints skipped to keep protected evidence durable.
    pub checkpoint_skips: u64,
    /// Provider tool-call batches rejected for duplicate call IDs.
    pub duplicate_tool_call_batches: u64,
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
            ToolRecoveryEvent::CheckpointSkipped => &mut self.checkpoint_skips,
            ToolRecoveryEvent::DuplicateToolCallBatch => &mut self.duplicate_tool_call_batches,
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
            ToolRecoveryEvent::CheckpointSkipped => self.checkpoint_skips,
            ToolRecoveryEvent::DuplicateToolCallBatch => self.duplicate_tool_call_batches,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Verifies the bounded recovery counters in summary JSON and Prometheus export.
    use super::{ToolRecoveryEvent, ToolRecoveryMetrics};
    use crate::metrics::MetricsCollector;

    /// Exports bounded recovery counters in summary JSON and Prometheus.
    #[test]
    fn tool_recovery_events_are_bounded_in_summary_json_and_prometheus() {
        let collector = MetricsCollector::new();
        collector.record_tool_recovery(ToolRecoveryEvent::StagedCall);
        collector.record_tool_recovery_events(ToolRecoveryEvent::InterruptedCall, 3);
        collector.record_tool_recovery(ToolRecoveryEvent::AmbiguousHistory);
        collector.record_tool_recovery(ToolRecoveryEvent::DuplicateToolCallBatch);
        collector.record_tool_recovery(ToolRecoveryEvent::CheckpointSkipped);

        let summary = collector.get_summary();
        assert_eq!(summary.tool_recovery.staged_calls, 1);
        assert_eq!(summary.tool_recovery.interrupted_calls, 3);
        assert_eq!(summary.tool_recovery.ambiguous_histories, 1);
        assert_eq!(summary.tool_recovery.duplicate_tool_call_batches, 1);
        assert_eq!(summary.tool_recovery.checkpoint_skips, 1);

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["tool_recovery"]["staged_calls"], 1);
        assert_eq!(json["tool_recovery"]["interrupted_calls"], 3);
        assert_eq!(json["tool_recovery"]["duplicate_tool_call_batches"], 1);
        // Labels are fixed variants, so no call content can reach the export.
        let rendered = json.to_string();
        assert!(!rendered.contains("tool_call_id"));

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains(r#"diapason_tool_recovery_total{event="staged_call"} 1"#));
        assert!(prometheus.contains(r#"diapason_tool_recovery_total{event="interrupted_call"} 3"#));
        assert!(prometheus.contains(r#"diapason_tool_recovery_total{event="duplicate_tool_call_batch"} 1"#));
        assert!(prometheus.contains(r#"diapason_tool_recovery_total{event="checkpoint_skipped"} 1"#));
    }

    /// Every recovery label is usable in a constant expression.
    ///
    /// This is the compile-time equivalent of a trybuild check: if `as_str`
    /// stopped being `const fn`, these constant items would fail to compile
    /// rather than only being caught by a runtime assertion.
    #[test]
    fn recovery_labels_are_const_evaluable() {
        const STAGED_CALL: &str = ToolRecoveryEvent::StagedCall.as_str();
        const AMBIGUOUS_HISTORY: &str = ToolRecoveryEvent::AmbiguousHistory.as_str();
        const DUPLICATE_TOOL_CALL_BATCH: &str = ToolRecoveryEvent::DuplicateToolCallBatch.as_str();
        const CHECKPOINT_SKIPPED: &str = ToolRecoveryEvent::CheckpointSkipped.as_str();

        assert_eq!(STAGED_CALL, "staged_call");
        assert_eq!(AMBIGUOUS_HISTORY, "ambiguous_history");
        assert_eq!(DUPLICATE_TOOL_CALL_BATCH, "duplicate_tool_call_batch");
        assert_eq!(CHECKPOINT_SKIPPED, "checkpoint_skipped");

        let labels = ToolRecoveryEvent::ALL.map(ToolRecoveryEvent::as_str);
        let unique: std::collections::HashSet<_> = labels.iter().collect();
        assert_eq!(unique.len(), labels.len(), "every recovery label must be unique");
    }

    /// Saturates counters at `u64::MAX` instead of wrapping.
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
