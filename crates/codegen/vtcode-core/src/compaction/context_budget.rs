//! Admit complete user instructions and bounded history against the model window.
//!
//! This module owns shared compaction request/output budgets and the typed
//! protected-instruction refusal. History composition stays in `history`;
//! optional-message selection stays in `bounded_history`.

use super::CompactionConfig;
use super::bounded_history::{protected_user_indices, select_bounded_history};
use super::protocol::continuity_tail;
use crate::llm::provider::{LLMProvider, Message};
use anyhow::Result;

/// Reserve model capacity for framing, memory and the next response.
const COMPACTION_CONTEXT_OVERHEAD_FRACTION_DENOMINATOR: usize = 8;
const COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS: usize = 512;

/// Reserve model-window space for framing and the next response. A provider
/// capacity of zero is unknown, so no hard history budget can be imposed here.
pub(super) fn compaction_history_budget(provider: &dyn LLMProvider, model: &str) -> Option<usize> {
    let context_size = provider.effective_context_size(model);
    (context_size > 0).then(|| {
        context_size
            .saturating_sub(context_size / COMPACTION_CONTEXT_OVERHEAD_FRACTION_DENOMINATOR)
            .saturating_sub(COMPACTION_CONTEXT_FIXED_OVERHEAD_TOKENS)
    })
}

/// Limit optional prefix retention to capacity left after the continuity tail;
/// complete protected instructions are checked separately against the hard budget.
pub(super) fn context_bounded_compaction_config(
    provider: &dyn LLMProvider,
    model: &str,
    history: &[Message],
    config: &CompactionConfig,
) -> CompactionConfig {
    let mut bounded = config.clone();
    if let Some(history_budget) = compaction_history_budget(provider, model) {
        let continuity_tokens = continuity_tail(history).iter().map(Message::estimate_tokens).sum::<usize>();
        bounded.retained_user_message_tokens = bounded
            .retained_user_message_tokens
            .min(history_budget.saturating_sub(continuity_tokens));
    }
    bounded
}

/// Apply the model-window budget to provider-native results as well as local
/// results. Native providers can return a large prefix or metadata-heavy tool
/// calls, so retaining the tail alone is not sufficient to guarantee that the
/// next request fits.
pub(super) fn bound_compacted_history_to_context(
    compacted: Vec<Message>,
    provider: &dyn LLMProvider,
    model: &str,
) -> Result<Vec<Message>> {
    let Some(history_budget) = compaction_history_budget(provider, model) else {
        return Ok(compacted);
    };
    ensure_protected_user_budget(&compacted, provider, model)?;
    if compacted.iter().map(Message::estimate_tokens).sum::<usize>() <= history_budget {
        return Ok(compacted);
    }
    Ok(select_bounded_history(&compacted, history_budget, true))
}

#[derive(Debug, thiserror::Error)]
#[error(
    "Cannot compact history: original task and latest user instructions require {protected_tokens} tokens, \
     exceeding the model context length budget of {history_budget}; user instructions were left unchanged"
)]
struct ProtectedUserBudgetExceeded {
    protected_tokens: usize,
    history_budget: usize,
}

/// Identify our own protected-instruction refusal through error context without
/// interpreting provider-controlled error text as an internal budget failure.
pub(crate) fn is_protected_user_budget_error(error: &anyhow::Error) -> bool {
    error.is::<ProtectedUserBudgetExceeded>()
}

/// Refuse before provider work if the complete first/latest source user turns
/// exceed the known model budget. This check never clips or mutates either turn.
pub(super) fn ensure_protected_user_budget(history: &[Message], provider: &dyn LLMProvider, model: &str) -> Result<()> {
    if let Some(history_budget) = compaction_history_budget(provider, model) {
        let protected_tokens = protected_user_indices(history)
            .iter()
            .map(|index| history[*index].estimate_tokens())
            .sum::<usize>();
        if protected_tokens > history_budget {
            return Err(ProtectedUserBudgetExceeded { protected_tokens, history_budget }.into());
        }
    }
    Ok(())
}

/// Borrow history that fits, or build a bounded copy for the summary request.
/// Full original/latest instructions retain priority over optional history.
pub(super) fn summary_history_for_request<'a>(
    history: &'a [Message],
    provider: &dyn LLMProvider,
    model: &str,
) -> Result<std::borrow::Cow<'a, [Message]>> {
    ensure_protected_user_budget(history, provider, model)?;
    if let Some(budget) = compaction_history_budget(provider, model)
        && history.iter().map(Message::estimate_tokens).sum::<usize>() > budget
    {
        return Ok(std::borrow::Cow::Owned(select_bounded_history(history, budget, true)));
    }
    Ok(std::borrow::Cow::Borrowed(history))
}
