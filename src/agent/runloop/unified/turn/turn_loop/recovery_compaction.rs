//! Bounded compaction before the post-tool tool-enabled retry.
//!
//! This boundary keeps recovery-specific history preservation and turn-index
//! rebasing out of the main turn loop. The request type is deliberately
//! explicit: recovery compaction may rewrite only the older prefix and must
//! receive the live memory-envelope and session-stat dependencies it updates.

use anyhow::Result;
use diapason_core::llm::provider as uni;

use crate::agent::runloop::unified::context_manager::ContextManager;
use crate::agent::runloop::unified::state::SessionStats;
use crate::agent::runloop::unified::turn::compaction::{
    CompactionContext, CompactionState, SessionMemoryEnvelopeUpdate, compact_history_for_recovery_in_place,
};

pub(super) struct RecoveryCompactionRequest<'a> {
    pub(super) history: &'a mut Vec<uni::Message>,
    pub(super) turn_history_start_len: &'a mut usize,
    pub(super) compaction_context: CompactionContext<'a>,
    pub(super) session_stats: &'a mut SessionStats,
    pub(super) context_manager: &'a mut ContextManager,
    pub(super) steering_update: SessionMemoryEnvelopeUpdate,
}

/// Compact the older history prefix while preserving the current request and
/// its tool results. Returns `true` only when the prefix was actually replaced.
pub(super) async fn compact_before_tool_enabled_retry(request: RecoveryCompactionRequest<'_>) -> Result<bool> {
    let RecoveryCompactionRequest {
        history,
        turn_history_start_len,
        compaction_context,
        session_stats,
        context_manager,
        steering_update,
    } = request;
    let original_history_len = history.len();
    let preserve_from_index = current_turn_preserve_index(history, *turn_history_start_len);
    let outcome = compact_history_for_recovery_in_place(
        compaction_context,
        CompactionState::new(history, session_stats, context_manager).with_steering_update(steering_update),
        preserve_from_index,
    )
    .await?;

    let Some(outcome) = outcome else {
        tracing::debug!(preserve_from_index, "Post-tool recovery compaction had no reducible prefix");
        return Ok(false);
    };

    let preserved_suffix_len = original_history_len.saturating_sub(preserve_from_index);
    *turn_history_start_len = outcome.compacted_len.saturating_sub(preserved_suffix_len);
    tracing::info!(
        original_len = outcome.original_len,
        compacted_len = outcome.compacted_len,
        preserve_from_index,
        turn_history_start_len = *turn_history_start_len,
        "Compacted the older prefix before the bounded tool-enabled recovery request"
    );
    Ok(true)
}

/// Return the beginning of the current request segment for recovery
/// compaction. Transient system notes can be appended after the user's
/// message, so using the loop's baseline directly could summarize the request
/// that the retry still needs verbatim.
pub(super) fn current_turn_preserve_index(history: &[uni::Message], turn_history_start_len: usize) -> usize {
    let turn_start = turn_history_start_len.min(history.len());
    history[..turn_start]
        .iter()
        .rposition(|message| message.role == uni::MessageRole::User)
        .unwrap_or(turn_start)
}
