//! Admission budgeting and the state carried through one compaction attempt.
//!
//! These types keep the orchestration in [`super`] focused on publishing
//! activity and committing results, while the sizing rules stay independently
//! testable.

use crate::zed::provider_runtime::ProviderRequestRuntime;
use crate::zed::types::SessionHandle;
use anyhow::Result;
use std::sync::Arc;
use vtcode_core::compaction::auto::AutoCompactionOutcome;
use vtcode_core::compaction::memory_envelope::effective_compaction_threshold;
use vtcode_core::config::VTCodeConfig;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::provider::{LLMProvider, Message, ToolDefinition};

const DEFAULT_OUTPUT_RESERVE_TOKENS: usize = 16_384;
const ADMISSION_SAFETY_MARGIN_TOKENS: usize = 1_024;

/// Estimate the prompt tokens the tool definitions occupy.
fn estimated_tool_tokens(tools: Option<&Arc<Vec<ToolDefinition>>>) -> usize {
    tools
        .and_then(|definitions| serde_json::to_vec(definitions.as_ref()).ok())
        .map_or(0, |encoded| encoded.len().div_ceil(4))
}

/// Estimate the prompt tokens for a message history and its tool definitions.
pub(in crate::zed::agent) fn estimated_prompt_tokens(
    messages: &[Message],
    tools: Option<&Arc<Vec<ToolDefinition>>>,
) -> usize {
    messages
        .iter()
        .map(Message::estimate_tokens)
        .sum::<usize>()
        .saturating_add(estimated_tool_tokens(tools))
}

/// Reserve output space and a safety margin from the provider context window.
///
/// Returns `None` when the provider reports no context window, because no
/// admission limit can be derived.
pub(in crate::zed::agent) fn admission_prompt_budget(provider: &dyn LLMProvider, model: &str) -> Option<usize> {
    let context_size = provider.effective_context_size(model);
    if context_size == 0 {
        return None;
    }

    let output_reserve = DEFAULT_OUTPUT_RESERVE_TOKENS.max(context_size / 10).min(context_size / 2);
    let safety_margin = ADMISSION_SAFETY_MARGIN_TOKENS.min(context_size / 10);
    Some(context_size.saturating_sub(output_reserve).saturating_sub(safety_margin).max(1))
}

/// Report whether a prompt has reached the tighter applicable budget.
#[cfg(test)]
pub(in crate::zed::agent) fn should_compact(provider: &dyn LLMProvider, model: &str, prompt_tokens: usize) -> bool {
    let threshold = effective_compaction_threshold(None, provider, model);
    let admission_budget = admission_prompt_budget(provider, model);
    threshold
        .into_iter()
        .chain(admission_budget)
        .min()
        .is_some_and(|budget| prompt_tokens >= budget)
}

#[derive(Clone, Copy)]
pub(in crate::zed::agent) struct CompactionDecision {
    pub(super) prompt_tokens: usize,
    pub(super) configured_threshold: Option<usize>,
    pub(super) admission_budget: Option<usize>,
    pub(super) force_compaction: bool,
}

impl CompactionDecision {
    /// Decide whether this prompt needs compaction, and why.
    ///
    /// Returns `None` while the prompt stays below every applicable budget.
    /// Admission pressure forces compaction even when the configured
    /// threshold has not been reached, because the request would otherwise be
    /// rejected by the provider.
    pub(super) fn for_prompt(
        vt_config: Option<&VTCodeConfig>,
        provider: &dyn LLMProvider,
        model: &str,
        prompt_tokens: usize,
    ) -> Option<Self> {
        let configured_threshold = effective_compaction_threshold(vt_config, provider, model);
        let admission_budget = admission_prompt_budget(provider, model);
        let trigger = configured_threshold.into_iter().chain(admission_budget).min();
        if trigger.is_none_or(|budget| prompt_tokens < budget) {
            return None;
        }

        let force_compaction = admission_budget.is_some_and(|budget| prompt_tokens >= budget)
            && configured_threshold.is_none_or(|threshold| prompt_tokens < threshold);
        Some(Self {
            prompt_tokens,
            configured_threshold,
            admission_budget,
            force_compaction,
        })
    }

    /// Report whether a token count has reached the admission budget.
    pub(super) fn is_over_admission_budget(self, prompt_tokens: usize) -> bool {
        self.admission_budget.is_some_and(|budget| prompt_tokens >= budget)
    }
}

pub(in crate::zed::agent) struct CompactionSessionSnapshot {
    pub(super) thread: ThreadRuntimeHandle,
    pub(super) session_id: String,
    pub(super) history: Vec<Message>,
    pub(super) auto_compact_suppressed: u8,
}

impl CompactionSessionSnapshot {
    /// Snapshot the session state that one compaction attempt works from.
    pub(super) fn load(session: &SessionHandle) -> Result<Self> {
        let data = session
            .data
            .lock()
            .map_err(|error| anyhow::anyhow!("ACP session lock poisoned: {error}"))?;
        Ok(Self {
            thread: data.thread.clone(),
            session_id: data.session_id.to_string(),
            history: data.thread.messages(),
            auto_compact_suppressed: data.auto_compact_suppressed,
        })
    }
}

pub(in crate::zed::agent) struct CompactionRun<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) provider: &'a dyn LLMProvider,
    pub(super) runtime: &'a ProviderRequestRuntime,
    pub(super) model: &'a str,
    pub(super) vt_config: &'a VTCodeConfig,
    pub(super) decision: CompactionDecision,
    pub(super) snapshot: CompactionSessionSnapshot,
}

pub(in crate::zed::agent) struct CompactionAttempt {
    pub(super) outcome: Option<AutoCompactionOutcome>,
    pub(super) thread: ThreadRuntimeHandle,
    pub(super) history: Vec<Message>,
}

pub(in crate::zed::agent) struct CompactionCommit<'a> {
    pub(super) session: &'a SessionHandle,
    pub(super) thread: ThreadRuntimeHandle,
    pub(super) history: Vec<Message>,
    pub(super) tools: Option<&'a Arc<Vec<ToolDefinition>>>,
    pub(super) decision: CompactionDecision,
}
