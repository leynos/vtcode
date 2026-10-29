//! Owns automatic eligibility, candidate preparation, and publication state.

use anyhow::Result;

use super::{AutoCompactionInput, AutoCompactionOutcome, try_two_pass_with_prefire};
use crate::compaction::memory_envelope::{
    MemoryEnvelopePersistence, dedup_repeated_file_reads_for_local_compaction, effective_compaction_threshold,
    persist_memory_envelope_async_with_update, strip_existing_memory_envelope,
};
use crate::compaction::{
    CompactionStrategy, SUPPRESS_NONE, classify_suppress_reason, compact_history_manual, manual_compaction_strategy,
};
use crate::exec::events::CompactionMode;
use crate::llm::provider::Message;

impl AutoCompactionInput<'_> {
    /// Apply enablement, threshold, and suppression before any automatic work.
    pub(super) fn is_eligible(&self) -> bool {
        if !self.vt_cfg.is_some_and(|cfg| cfg.agent.harness.auto_compaction_enabled) {
            return false;
        }
        let Some(threshold) = effective_compaction_threshold(self.vt_cfg, self.provider, self.model) else {
            return false;
        };
        if self.current_token_usage < threshold && !self.force_compaction {
            return false;
        }
        *self.auto_compact_suppressed == SUPPRESS_NONE
    }

    /// Forced or explicitly requested summaries bypass the legacy segment floor.
    fn skips_small_segment(&self, message_count: usize) -> bool {
        if self.force_compaction {
            return false;
        }
        if self.engine_cfg.always_summarize {
            return false;
        }
        message_count <= self.engine_cfg.keep_last_messages
    }
}

/// Keeps the original archive input separate from unpublished candidates.
pub(super) struct AutoCompactionAttempt<'a> {
    input: AutoCompactionInput<'a>,
    original_history: Vec<Message>,
    pub(super) selected_mode: CompactionMode,
}

pub(super) struct CompactionCandidate {
    history: Vec<Message>,
    mode: CompactionMode,
}

/// Persistence has succeeded; this history can now replace the live sequence.
pub(super) struct PreparedCompaction {
    history: Vec<Message>,
    outcome: AutoCompactionOutcome,
}

impl PreparedCompaction {
    /// Replace live history only after candidate persistence has succeeded.
    pub(super) fn publish(self, history: &mut Vec<Message>) -> AutoCompactionOutcome {
        *history = self.history;
        self.outcome
    }
}

impl<'a> AutoCompactionAttempt<'a> {
    /// Snapshot archive input while removing the prior injected envelope.
    pub(super) fn new(input: AutoCompactionInput<'a>, history: &[Message]) -> Self {
        let mut original_history = history.to_vec();
        strip_existing_memory_envelope(&mut original_history);
        Self {
            input,
            original_history,
            selected_mode: CompactionMode::Unknown,
        }
    }

    /// Try cached prefire before consulting capabilities or choosing fallback.
    pub(super) async fn produce_candidate(&mut self) -> Result<Option<CompactionCandidate>> {
        if let Some(prefire) = self.input.prefire {
            self.selected_mode = CompactionMode::Local;
            if let Some(history) = try_two_pass_with_prefire(
                prefire,
                self.input.provider,
                self.input.model,
                &self.original_history,
                &self.input.engine_cfg,
            )
            .await?
            {
                return Ok(Some(CompactionCandidate { history, mode: CompactionMode::Local }));
            }
        }

        let strategy = manual_compaction_strategy(self.input.provider, self.input.model);
        let summary_history = match strategy {
            CompactionStrategy::Local => {
                self.selected_mode = CompactionMode::Local;
                dedup_repeated_file_reads_for_local_compaction(&self.original_history)
            }
            CompactionStrategy::NativeStandalone | CompactionStrategy::NativeInline => {
                self.selected_mode = CompactionMode::Provider;
                self.original_history.clone()
            }
        };
        if self.input.skips_small_segment(summary_history.len()) {
            return Ok(None);
        }
        let (history, mode) = compact_history_manual(
            self.input.provider,
            self.input.model,
            &summary_history,
            &self.input.engine_cfg,
            &self.input.manual_options,
        )
        .await?;
        if history == summary_history {
            return Ok(None);
        }
        Ok(Some(CompactionCandidate { history, mode }))
    }

    /// Persist the original history and inject its envelope before publication.
    pub(super) async fn persist_candidate(&self, mut candidate: CompactionCandidate) -> Result<PreparedCompaction> {
        let envelope = persist_memory_envelope_async_with_update(
            self.input.workspace_root,
            self.input.session_id,
            self.input.vt_cfg,
            &self.original_history,
            self.input.touched_files,
            &mut candidate.history,
            MemoryEnvelopePersistence::PersistToDisk,
            self.input.placement,
            None,
            self.input.steering_update,
        )
        .await?;
        let outcome = AutoCompactionOutcome {
            original_len: self.original_history.len(),
            compacted_len: candidate.history.len(),
            mode: candidate.mode,
            history_artefact_path: envelope.as_ref().and_then(|item| item.history_artefact_path.clone()),
            envelope,
        };
        Ok(PreparedCompaction { history: candidate.history, outcome })
    }

    /// Preserve sticky failures and clear suppression only after publication.
    pub(super) fn update_suppression(&mut self, result: &Result<Option<AutoCompactionOutcome>>) {
        match result {
            Ok(Some(_)) => *self.input.auto_compact_suppressed = SUPPRESS_NONE,
            Ok(None) => {}
            Err(error) => {
                let reason = classify_suppress_reason(&error.root_cause().to_string());
                if *self.input.auto_compact_suppressed == SUPPRESS_NONE {
                    *self.input.auto_compact_suppressed = reason.suppress_state();
                }
            }
        }
    }
}
