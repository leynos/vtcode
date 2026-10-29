//! Unified automatic compaction orchestration.
//!
//! This is the single entry point used by every runtime (the binary unified
//! runloop and the `vtcode-core` `AgentRunner`) for *automatic* context
//! compaction: when token pressure crosses the configured threshold, the
//! conversation trace is compressed (provider-native or local LLM summary),
//! a recoverable history artifact is written, and a session memory envelope is
//! built and injected so the model keeps its conversational continuity.

use std::path::Path;

use anyhow::Result;

use super::telemetry::{AutoCompactionMetricSink, AutoCompactionTelemetry};
use crate::compaction::memory_envelope::{MemoryEnvelopePlacement, SessionMemoryEnvelopeUpdate};
use crate::compaction::two_pass::fingerprint_prefix;
use crate::compaction::{
    CompactionConfig, ManualCompactionOptions, build_local_compacted_history, build_summary_prompt,
};
use crate::exec::events::CompactionMode;
use crate::llm::provider::{LLMProvider, LLMRequest, Message};
use vtcode_config::loader::VTCodeConfig;

mod attempt;

use attempt::AutoCompactionAttempt;

/// Result of a successful automatic compaction pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCompactionOutcome {
    pub original_len: usize,
    pub compacted_len: usize,
    pub mode: CompactionMode,
    pub history_artefact_path: Option<String>,
    /// The session memory envelope produced during compaction.
    /// Used by the caller to write persistent checkpoints.
    pub envelope: Option<crate::compaction::memory_envelope::SessionMemoryEnvelope>,
}

/// Inputs for [`auto_compact_messages`].
pub struct AutoCompactionInput<'a> {
    pub provider: &'a dyn LLMProvider,
    pub model: &'a str,
    pub session_id: &'a str,
    pub workspace_root: &'a Path,
    pub vt_cfg: Option<&'a VTCodeConfig>,
    /// Current estimated token usage of the live history (before compaction).
    pub current_token_usage: usize,
    /// Files touched so far this session (enrich the memory envelope).
    pub touched_files: &'a [String],
    /// Engine configuration (thresholds, retention, summary prompt overrides).
    pub engine_cfg: CompactionConfig,
    /// Manual compaction options (instructions, max output, reasoning, verbosity).
    pub manual_options: ManualCompactionOptions,
    /// Where to place the injected memory envelope in the compacted history.
    pub placement: MemoryEnvelopePlacement,
    /// Prefire two-pass state (background NOTE₁ cache).
    pub prefire: Option<&'a crate::compaction::PrefireState>,
    /// Mutable auto-compaction suppression state. `SUPPRESS_NONE` allows
    /// compaction; any other value gates automatic compaction until cleared
    /// by success, model switch, or explicit `/compact`.
    pub auto_compact_suppressed: &'a mut u8,
    /// Force a pending soft-threshold compaction at the next outer turn
    /// boundary. This is set only by the runloop boundary, never by a tool
    /// loop, so no hidden model call is introduced mid-turn.
    pub force_compaction: bool,
    /// Live steering state to snapshot with the compaction envelope.
    pub steering_update: Option<&'a SessionMemoryEnvelopeUpdate>,
}

/// Compress `history` in place when automatic compaction should fire.
///
/// Returns `None` when compaction is disabled, the history is below the trigger
/// threshold, or the engine produced no change. On a successful pass the
/// compacted history (with envelope injected) replaces `history` and an
/// [`AutoCompactionOutcome`] is returned; the caller is responsible for
/// emitting the `thread.compact_boundary` event and resetting token accounting.
pub async fn auto_compact_messages(
    input: AutoCompactionInput<'_>,
    history: &mut Vec<Message>,
) -> Result<Option<AutoCompactionOutcome>> {
    auto_compact_messages_with_metrics(input, history, &AutoCompactionMetricSink::Process).await
}

/// Observe only eligible automatic passes; skipped triggers emit no attempt.
async fn auto_compact_messages_with_metrics(
    input: AutoCompactionInput<'_>,
    history: &mut Vec<Message>,
    metrics: &AutoCompactionMetricSink,
) -> Result<Option<AutoCompactionOutcome>> {
    if !input.is_eligible() {
        return Ok(None);
    }

    let observation = AutoCompactionTelemetry::start(metrics, history);
    let mut attempt = AutoCompactionAttempt::new(input, history);
    let compaction_result: Result<Option<AutoCompactionOutcome>> = async {
        let Some(candidate) = attempt.produce_candidate().await? else {
            return Ok(None);
        };
        let prepared = attempt.persist_candidate(candidate).await?;
        Ok(Some(prepared.publish(history)))
    }
    .await;

    attempt.update_suppression(&compaction_result);
    observation.finish(&compaction_result, history, attempt.selected_mode);
    compaction_result
}

/// Attempt two-pass compaction using a prefire-cached NOTE₁.
///
/// Returns `Some(compacted_history)` if the cache is valid and pass-2 produces
/// a non-degenerate summary. `None` means the caller should fall back to
/// single-pass compaction.
async fn try_two_pass_with_prefire(
    prefire: &crate::compaction::PrefireState,
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
) -> Result<Option<Vec<Message>>> {
    crate::compaction::ensure_protected_user_budget(history, provider, model)?;
    let cache = match prefire.take() {
        Some(cache) => cache,
        None => return Ok(None),
    };

    if cache.prefix_len == 0 || cache.prefix_len > history.len() {
        return Ok(None);
    }

    if cache.model_slug != model {
        return Ok(None);
    }

    let prefix = &history[..cache.prefix_len];
    if fingerprint_prefix(prefix) != cache.fingerprint {
        return Ok(None);
    }

    let tail = &history[cache.prefix_len..];
    let prompt = build_summary_prompt(tail, &config.summary_prompt);
    let pass2_history = crate::compaction::two_pass::build_two_pass_pass2_history(prefix, tail, &cache.note1, &prompt);

    // The cached note and duplicated tail prompt can exceed the model window.
    // Fall back to the bounded single-pass path instead of sending a doomed
    // pass-2 request or clipping its user-carried instructions.
    if crate::compaction::compaction_history_budget(provider, model)
        .is_some_and(|budget| pass2_history.iter().map(Message::estimate_tokens).sum::<usize>() > budget)
    {
        return Ok(None);
    }

    let request = LLMRequest {
        messages: std::sync::Arc::new(pass2_history),
        model: model.to_string(),
        ..Default::default()
    };

    let response = provider.generate(request).await?;
    let note2 = response.content.unwrap_or_default().trim().to_string();

    if note2.trim().is_empty() {
        return Ok(None);
    }

    let effective_config = crate::compaction::context_bounded_compaction_config(provider, model, history, config);
    let compacted = build_local_compacted_history(history, &note2, &effective_config, true);

    Ok(Some(crate::compaction::bound_compacted_history_to_context(compacted, provider, model)?))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod prefire_tests;
