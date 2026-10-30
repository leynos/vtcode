//! Select bounded history while preserving complete original/latest user turns.
//!
//! This module owns optional-message budgets, summary sharing, atomic unit
//! admission and bounded previews. Protocol completeness and suffix selection
//! belong to `protocol`; model-window admission remains in the parent module.

use crate::config::constants::LEADING_SUMMARY_ALLOWANCE_TOKENS;
use crate::llm::provider::{Message, MessageContent, MessageRole};
use crate::llm::utils::truncate_to_token_limit;

const COMPACTION_PREVIEW_MARKER: &str = "[Compaction preview: earlier content omitted]\n";

/// Return first/latest user positions, counting a sole user only once.
/// Protect sequence positions rather than text: identical steering messages
/// may carry distinct intent IDs and must survive as separate turns.
pub(super) fn protected_user_indices(history: &[Message]) -> Vec<usize> {
    let mut users = history
        .iter()
        .enumerate()
        .filter_map(|(index, message)| (message.role == MessageRole::User).then_some(index));
    let Some(first) = users.next() else {
        return Vec::new();
    };
    let last = users.next_back().unwrap_or(first);
    if first == last { vec![first] } else { vec![first, last] }
}

/// Reserve complete user instructions, then the summary and newest coherent
/// assistant/result units. The soft continuity allowance can expand for a user
/// anchor; only the model-context finalizer imposes the hard admission budget.
pub(super) fn select_bounded_history(history: &[Message], token_budget: usize, keep_summary: bool) -> Vec<Message> {
    let mut selection = BoundedHistorySelection::with_protected_users(history, token_budget);
    if keep_summary {
        selection.retain_leading_summaries(history);
    }
    selection.retain_recent_units(history);
    selection.into_history()
}

struct IndexedHistoryMessage {
    source_index: usize,
    message: Message,
}

/// Own optional-message admission after reserving the complete user anchors.
struct BoundedHistorySelection {
    selected: Vec<IndexedHistoryMessage>,
    remaining_tokens: usize,
}

impl BoundedHistorySelection {
    /// Clone complete source anchors and charge them before optional admission.
    fn with_protected_users(history: &[Message], token_budget: usize) -> Self {
        let selected: Vec<_> = protected_user_indices(history)
            .into_iter()
            .map(|source_index| IndexedHistoryMessage {
                source_index,
                message: history[source_index].clone(),
            })
            .collect();
        let used = selected.iter().map(|entry| entry.message.estimate_tokens()).sum::<usize>();
        Self {
            selected,
            remaining_tokens: token_budget.saturating_sub(used),
        }
    }

    /// Share one fixed allowance across leading summary tiers, reserving an
    /// equal share for each remaining tier before admitting its bounded preview.
    fn retain_leading_summaries(&mut self, history: &[Message]) {
        let summary_count = history.iter().take_while(|message| message.role == MessageRole::System).count();
        let mut summary_allowance = self.remaining_tokens.min(LEADING_SUMMARY_ALLOWANCE_TOKENS);
        for (index, summary) in history[..summary_count].iter().enumerate() {
            // Reserve a share for each remaining summary so an oversized
            // abstract cannot consume the detail summary's entire allowance.
            let summary_budget = summary_allowance / (summary_count - index);
            let preview = bounded_message_preview(summary, summary_budget);
            let tokens = preview.estimate_tokens();
            if tokens <= summary_budget {
                self.remaining_tokens -= tokens;
                summary_allowance -= tokens;
                self.selected
                    .push(IndexedHistoryMessage { source_index: index, message: preview });
            }
        }
    }

    /// Admit newest assistant/result units atomically and user turns whole.
    /// Standalone tool results cannot introduce an orphan into selected history.
    fn retain_recent_units(&mut self, history: &[Message]) {
        for unit_range in protocol_unit_ranges(history).into_iter().rev() {
            let start = unit_range.start;
            if self.selected.iter().any(|entry| entry.source_index == start)
                || matches!(history[start].role, MessageRole::System | MessageRole::Tool)
            {
                continue;
            }
            let Some(bounded) = bounded_history_unit(&history[unit_range], self.remaining_tokens) else {
                continue;
            };
            let tokens = bounded.iter().map(Message::estimate_tokens).sum::<usize>();
            if tokens <= self.remaining_tokens {
                self.remaining_tokens -= tokens;
                self.selected.extend(
                    bounded
                        .into_iter()
                        .enumerate()
                        .map(|(offset, message)| IndexedHistoryMessage { source_index: start + offset, message }),
                );
            }
        }
    }

    fn into_history(mut self) -> Vec<Message> {
        self.selected.sort_by_key(|entry| entry.source_index);
        self.selected.into_iter().map(|entry| entry.message).collect()
    }
}

/// Keep consecutive results attached to their assistant, including legacy
/// results whose originating call metadata was not persisted.
fn protocol_unit_ranges(history: &[Message]) -> Vec<std::ops::Range<usize>> {
    let mut units = Vec::new();
    let mut cursor = 0;
    while cursor < history.len() {
        let start = cursor;
        cursor += 1;
        if history[start].role == MessageRole::Assistant {
            while cursor < history.len() && history[cursor].role == MessageRole::Tool {
                cursor += 1;
            }
        }
        units.push(start..cursor);
    }
    units
}

/// Ordinary user turns are admitted whole; only assistant/result units can
/// share the remaining allowance through previews.
fn bounded_history_unit(unit: &[Message], token_budget: usize) -> Option<Vec<Message>> {
    let tokens = unit.iter().map(Message::estimate_tokens).sum::<usize>();
    if tokens <= token_budget {
        return Some(unit.to_vec());
    }
    if unit.first().is_some_and(|message| message.role == MessageRole::User) {
        return None;
    }
    let per_message = token_budget / unit.len();
    Some(
        unit.iter()
            .map(|message| bounded_message_preview(message, per_message))
            .collect(),
    )
}

/// Bound an already validated protocol group without retaining summary tiers.
pub(super) fn bounded_protocol_group(group: &[Message], token_budget: usize) -> Vec<Message> {
    select_bounded_history(group, token_budget, false)
}

/// Reduce optional payloads while retaining user turns and correlation fields.
/// A minimal tool envelope may still exceed its share; unit admission rejects
/// that whole preview batch rather than emitting an over-budget partial batch.
fn bounded_message_preview(message: &Message, token_budget: usize) -> Message {
    if message.role == MessageRole::User || message.estimate_tokens() <= token_budget {
        return message.clone();
    }
    let mut preview = message.clone();
    let available = token_budget.saturating_sub(4);
    preview.content = preview_visible_content(message, available);
    trim_preview_metadata(&mut preview, available);
    if preview.estimate_tokens() > token_budget {
        minimize_preview_payload(&mut preview);
    }
    preview
}

/// Label omitted visible content, charging the label against the text allowance.
fn preview_visible_content(message: &Message, available: usize) -> MessageContent {
    let marker_tokens = COMPACTION_PREVIEW_MARKER.len().div_ceil(4);
    let text = if available >= marker_tokens {
        format!(
            "{COMPACTION_PREVIEW_MARKER}{}",
            truncate_to_token_limit(message.content.as_text().as_ref(), available - marker_tokens)
        )
    } else {
        String::new()
    };
    MessageContent::Text(text)
}

/// Metadata contributes tokens even when visible content is empty. Retain
/// correlation IDs and function names while bounding optional payloads.
fn trim_preview_metadata(preview: &mut Message, available: usize) {
    preview.reasoning = preview
        .reasoning
        .take()
        .map(|reasoning| truncate_to_token_limit(&reasoning, available.min(1_024)));
    preview.reasoning_details = None;
    preview.metadata = None;
    preview.origin_tool = None;
    if let Some(tool_calls) = preview.tool_calls.as_mut() {
        for call in tool_calls {
            if let Some(function) = call.function.as_mut()
                && function.arguments.len() > available.saturating_mul(4)
            {
                function.arguments = "{}".to_string();
            }
            if let Some(text) = call.text.as_mut() {
                *text = truncate_to_token_limit(text, available.min(1_024));
            }
            call.thought_signature = None;
        }
    }
}

/// Last resort after normal previewing: retain the omission marker and the
/// minimal correlatable tool envelope, even when that envelope cannot fit.
fn minimize_preview_payload(preview: &mut Message) {
    preview.content = MessageContent::Text(COMPACTION_PREVIEW_MARKER.trim_end().to_string());
    preview.reasoning = None;
    preview.reasoning_details = None;
    if let Some(tool_calls) = preview.tool_calls.as_mut() {
        for call in tool_calls {
            if let Some(function) = call.function.as_mut() {
                function.arguments = "{}".to_string();
            }
            call.text = None;
            call.thought_signature = None;
        }
    }
}
