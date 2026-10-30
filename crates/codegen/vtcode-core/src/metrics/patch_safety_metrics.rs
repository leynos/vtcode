//! Bounded counters for `apply_patch` version and no-op decisions.
//!
//! Every dimension below is a fixed enum variant. Paths, patch signatures,
//! payloads, and error strings never become a label, so a session that edits
//! private files can export this snapshot without leaking their content.

use serde::{Deserialize, Serialize};

/// The outcome of one `apply_patch` version precondition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchPreconditionOutcome {
    /// The supplied `expected_content_hash` matched the current file.
    Matched,
    /// The file changed after it was read, so the patch was rejected.
    ContentHashMismatch,
    /// `expected_content_hash` was supplied for an add-only or multi-file patch.
    InvalidScope,
    /// `expected_content_hash` was not a canonical `sha256:<hex>` value.
    MalformedHash,
}

impl PatchPreconditionOutcome {
    /// Stable Prometheus label and log field for this outcome.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::ContentHashMismatch => "content_hash_mismatch",
            Self::InvalidScope => "invalid_scope",
            Self::MalformedHash => "malformed_hash",
        }
    }
}

/// The outcome of one evaluation of the repeated no-op ladder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchNoOpOutcome {
    /// The patch changes at least one file.
    Execute,
    /// The first identical no-op in a row, reported as success.
    NoOpFirst,
    /// The second identical no-op in a row, reported with a warning.
    NoOpRepeated,
    /// The third identical no-op in a row, refused.
    NoOpBlocked,
}

impl PatchNoOpOutcome {
    /// Stable Prometheus label and log field for this decision.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Execute => "execute",
            Self::NoOpFirst => "no_op_first",
            Self::NoOpRepeated => "no_op_repeated",
            Self::NoOpBlocked => "no_op_blocked",
        }
    }
}

/// Bounded `apply_patch` safety counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PatchSafetyMetrics {
    /// Version preconditions that matched the current file.
    pub precondition_matched: u64,
    /// Version preconditions rejected because the file had changed.
    pub precondition_content_hash_mismatch: u64,
    /// Preconditions rejected because the patch had no single source file.
    pub precondition_invalid_scope: u64,
    /// Preconditions rejected because the supplied hash was malformed.
    pub precondition_malformed_hash: u64,
    /// Patches that changed at least one file.
    pub no_op_execute: u64,
    /// First identical no-op in a row.
    pub no_op_first: u64,
    /// Second identical no-op in a row.
    pub no_op_repeated: u64,
    /// Third identical no-op in a row, refused.
    pub no_op_blocked: u64,
    /// Number of evaluated `apply_patch` requests.
    pub evaluations: u64,
    /// Total evaluation time in milliseconds.
    pub total_duration_ms: u64,
}

impl PatchSafetyMetrics {
    /// Count one version-precondition outcome.
    pub fn record_precondition(&mut self, outcome: PatchPreconditionOutcome) {
        let counter = match outcome {
            PatchPreconditionOutcome::Matched => &mut self.precondition_matched,
            PatchPreconditionOutcome::ContentHashMismatch => &mut self.precondition_content_hash_mismatch,
            PatchPreconditionOutcome::InvalidScope => &mut self.precondition_invalid_scope,
            PatchPreconditionOutcome::MalformedHash => &mut self.precondition_malformed_hash,
        };
        *counter = counter.saturating_add(1);
    }

    /// Count one no-op ladder decision.
    pub fn record_no_op(&mut self, decision: PatchNoOpOutcome) {
        let counter = match decision {
            PatchNoOpOutcome::Execute => &mut self.no_op_execute,
            PatchNoOpOutcome::NoOpFirst => &mut self.no_op_first,
            PatchNoOpOutcome::NoOpRepeated => &mut self.no_op_repeated,
            PatchNoOpOutcome::NoOpBlocked => &mut self.no_op_blocked,
        };
        *counter = counter.saturating_add(1);
    }

    /// Count one completed evaluation and add its duration.
    pub fn record_evaluation(&mut self, duration_ms: u64) {
        self.evaluations = self.evaluations.saturating_add(1);
        self.total_duration_ms = self.total_duration_ms.saturating_add(duration_ms);
    }

    /// Average evaluation time, or zero before the first evaluation.
    pub fn avg_duration_ms(&self) -> u64 {
        self.total_duration_ms.checked_div(self.evaluations).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{PatchNoOpOutcome, PatchPreconditionOutcome, PatchSafetyMetrics};
    use crate::metrics::MetricsCollector;

    #[test]
    fn patch_safety_decisions_are_bounded_in_summary_json_and_prometheus() {
        let collector = MetricsCollector::new();
        collector.record_patch_precondition(PatchPreconditionOutcome::ContentHashMismatch);
        collector.record_patch_precondition(PatchPreconditionOutcome::Matched);
        collector.record_patch_no_op(PatchNoOpOutcome::NoOpRepeated);
        collector.record_patch_evaluation(40);

        let summary = collector.get_summary();
        assert_eq!(summary.patch_safety.precondition_matched, 1);
        assert_eq!(summary.patch_safety.precondition_content_hash_mismatch, 1);
        assert_eq!(summary.patch_safety.no_op_repeated, 1);
        assert_eq!(summary.patch_safety.evaluations, 1);
        assert_eq!(summary.patch_safety.avg_duration_ms(), 40);

        let json = collector.export_json().expect("metrics JSON should serialize");
        assert_eq!(json["patch_safety"]["no_op_repeated"], 1);
        // Labels are fixed variants, so no payload text can reach the export.
        let rendered = json.to_string();
        assert!(!rendered.contains("*** Begin Patch"));
        assert!(!rendered.contains("sha256:"));

        let prometheus = collector.export_prometheus();
        assert!(prometheus.contains(r#"vtcode_apply_patch_precondition_total{outcome="content_hash_mismatch"} 1"#));
        assert!(prometheus.contains(r#"vtcode_apply_patch_no_op_total{decision="no_op_repeated"} 1"#));
        assert!(prometheus.contains("vtcode_apply_patch_avg_duration_ms 40"));
    }

    #[test]
    fn average_duration_is_zero_before_any_evaluation() {
        assert_eq!(PatchSafetyMetrics::default().avg_duration_ms(), 0);
    }
}
