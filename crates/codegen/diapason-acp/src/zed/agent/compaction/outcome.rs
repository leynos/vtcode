//! Terminal outcome labels for automatic ACP compaction.

/// Name the terminal outcome reported to logs and tracing.
///
/// A failure takes precedence over a successful compaction, so a run that
/// compacted before failing still reports as failed to callers.
pub(super) const fn compaction_outcome_label(failed: bool, compacted: bool) -> &'static str {
    match (failed, compacted) {
        (true, _) => "failed",
        (false, true) => "compacted",
        (false, false) => "no_change",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assert failure wins over compaction and non-compaction.
    #[test]
    fn failure_takes_precedence_over_compaction_state() {
        assert_eq!(compaction_outcome_label(true, true), "failed");
        assert_eq!(compaction_outcome_label(true, false), "failed");
        assert_eq!(compaction_outcome_label(false, true), "compacted");
        assert_eq!(compaction_outcome_label(false, false), "no_change");
    }
}
