//! Compose local and provider-compacted histories from summaries and source turns.
//!
//! Continuity selection owns latest steering, retention owns optional prefix
//! history, and source user messages remain authoritative over provider rewrites.
//! This module does not admit model requests or publish live history.

use super::protocol::{complete_protocol_prefix, continuity_tail, continuity_tail_selection, split_continuity_history};
use super::{CompactionConfig, SUMMARY_PREFIX};
use crate::llm::provider::{Message, MessageRole};
use chrono::{DateTime, Utc};
use retention::{UserRetentionPolicy, collect_retained_user_messages};
use std::fmt::Write;

pub(super) mod retention;

/// Format visible history for summarization without provider reasoning traces.
/// Include the provided work independently of later retention decisions;
/// request-budget admission remains the caller's responsibility.
pub(crate) fn build_summary_prompt(history: &[Message], instructions: &str) -> String {
    // Pre-size for the header plus every (non-empty) message body, avoiding
    // repeated reallocations while the summary prompt is assembled.
    let estimated_len =
        instructions.len() + history.iter().map(|m| m.content.as_text().len()).sum::<usize>() + history.len() * 16;
    let mut formatted = String::with_capacity(estimated_len);
    let now: DateTime<Utc> = Utc::now();
    let _ = writeln!(&mut formatted, "Summary requested at {}.\n{}", now.to_rfc3339(), instructions);

    for message in history {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let content = message.content.as_text();
        if content.trim().is_empty() {
            continue;
        }
        let _ = writeln!(&mut formatted, "\n[{}]\n{}", role, content.trim());
    }

    formatted
}

/// Assemble a summary, optional retained prefix and complete continuity tail.
/// Read optional retention settings from the resolved compaction configuration.
/// When the tail owns the latest user turn, only the prefix's original task is
/// protected; its other user turns remain subject to optional retention limits.
pub(crate) fn build_local_compacted_history(
    history: &[Message],
    summary: &str,
    config: &CompactionConfig,
    include_continuity_tail: bool,
) -> Vec<Message> {
    let (retention_history, continuity) = if include_continuity_tail {
        split_continuity_history(history)
    } else {
        (history, Vec::new())
    };
    let retention_policy = if continuity.is_empty() {
        UserRetentionPolicy::WholeHistory
    } else {
        UserRetentionPolicy::BeforeContinuityTail
    };
    let retained_users = collect_retained_user_messages(
        retention_history,
        config.retained_user_message_tokens,
        config.retained_user_messages,
        retention_policy,
    );
    let mut new_history = Vec::with_capacity(retained_users.len().saturating_add(1));
    new_history.push(Message::system(format!("{SUMMARY_PREFIX}{}", summary.trim())));
    new_history.extend(retained_users);

    // Continuity anchor: retain the newest complete protocol groups verbatim
    // within the fixed tail budget. Duplicate message text is still valid
    // across turns, so preserve the sequence by index rather than deduplicating
    // on role/content.
    if include_continuity_tail {
        for message in continuity {
            new_history.push(message);
        }
    }
    new_history
}

/// Apply the resolved retention configuration to a generated local summary.
pub(super) fn build_summary_compacted_history(
    history: &[Message],
    summary: impl AsRef<str>,
    config: &CompactionConfig,
    include_continuity_tail: bool,
) -> Vec<Message> {
    build_local_compacted_history(history, summary.as_ref(), config, include_continuity_tail)
}

/// Keep the provider's compacted prefix, but apply the same bounded protocol
/// tail rules used by local compaction. A provider may return a summary only;
/// that remains valid, while malformed trailing tool calls are discarded.
pub(super) fn normalize_provider_compacted_history(
    compacted: Vec<Message>,
    fallback_history: &[Message],
) -> Vec<Message> {
    // Echoing the source is a provider no-op. Preserve its exact ordering,
    // repeated tool reads and metadata so callers can recognize that no
    // compaction occurred. Incomplete echoed tool batches still require
    // normalization, and the caller still enforces the model budget.
    if compacted == fallback_history && complete_protocol_prefix(fallback_history) == fallback_history.len() {
        return compacted;
    }

    // Provider compaction may rewrite or omit user instructions. Its prefix is
    // useful summary state, but the source history owns the authoritative task
    // and sequence of steering messages.
    let mut normalized = compacted
        .into_iter()
        .filter(|message| {
            message.role == MessageRole::System
                || (message.role == MessageRole::Assistant && message.tool_calls.as_ref().is_none_or(Vec::is_empty))
        })
        .collect::<Vec<_>>();
    let tail_start = continuity_tail_selection(fallback_history)
        .map(|selection| selection.range.start)
        .unwrap_or(fallback_history.len());
    if let Some(first) = fallback_history.iter().position(|message| message.role == MessageRole::User)
        && first < tail_start
    {
        normalized.push(fallback_history[first].clone());
    }
    normalized.extend(continuity_tail(fallback_history));
    normalized
}
