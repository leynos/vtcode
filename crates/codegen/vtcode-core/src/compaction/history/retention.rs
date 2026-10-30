//! Rank and admit optional historical turns after reserving authoritative users.
//!
//! Scoring determines priority, count/token allowances limit optional admission,
//! source indices restore chronology, and coherence retains required tool results.
//! Tail-owned latest steering is never reclassified as a protected prefix turn.

use super::super::bounded_history::protected_user_indices;
use crate::llm::provider::{Message, MessageRole};

/// Which authoritative user anchors are still owned by the input slice.
#[derive(Clone, Copy)]
pub(in crate::compaction) enum UserRetentionPolicy {
    /// The unsplit history owns both the original task and latest steering.
    WholeHistory,
    /// The continuity tail owns latest steering; this prefix owns only the task.
    BeforeContinuityTail,
}

/// Retain complete user anchors before scoring optional historical messages.
/// Protected turns may exceed the soft count/token limits; all other user
/// messages must fit whole, with omitted material carried by the summary.
pub(in crate::compaction) fn collect_retained_user_messages(
    history: &[Message],
    token_budget: usize,
    max_messages: usize,
    policy: UserRetentionPolicy,
) -> Vec<Message> {
    let user_ranked = rank_candidates(history, CandidateKind::User);
    let mut selection = RetainedHistorySelection::with_protected_users(history, token_budget, max_messages, policy);
    selection.admit_ranked(&user_ranked);
    if selection.selected_indices.len() < max_messages && selection.remaining_tokens > 0 {
        let non_user_ranked = rank_candidates(history, CandidateKind::NonUser);
        selection.admit_ranked(&non_user_ranked);
    }
    selection.into_coherent_history(history)
}

/// Keep user priority separate from the later optional non-user phase.
#[derive(Clone, Copy)]
enum CandidateKind {
    User,
    NonUser,
}

struct ScoredHistoryMessage<'a> {
    source_index: usize,
    score: f64,
    message: &'a Message,
}

/// Rank one eligibility class, preserving source order when scores compare equal.
fn rank_candidates(history: &[Message], kind: CandidateKind) -> Vec<ScoredHistoryMessage<'_>> {
    let mut ranked = history
        .iter()
        .enumerate()
        .filter(|(_, message)| match kind {
            CandidateKind::User => message.role == MessageRole::User && !message.content.trim().is_empty(),
            CandidateKind::NonUser => match message.role {
                MessageRole::Assistant | MessageRole::Tool => is_retainable_message(message),
                MessageRole::User | MessageRole::System => false,
            },
        })
        .map(|(source_index, message)| ScoredHistoryMessage {
            source_index,
            score: score_message(message, source_index, history.len()),
            message,
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal));
    ranked
}

/// Own source identities and the remaining optional count/token allowances.
struct RetainedHistorySelection {
    selected_indices: Vec<usize>,
    remaining_tokens: usize,
    max_messages: usize,
}

impl RetainedHistorySelection {
    /// Reserve authoritative anchors whole before either ranked admission phase.
    fn with_protected_users(
        history: &[Message],
        token_budget: usize,
        max_messages: usize,
        policy: UserRetentionPolicy,
    ) -> Self {
        let mut selected_indices = protected_user_indices(history);
        if matches!(policy, UserRetentionPolicy::BeforeContinuityTail) {
            selected_indices.truncate(1);
        }
        let protected_tokens = selected_indices
            .iter()
            .map(|index| history[*index].estimate_tokens())
            .sum::<usize>();
        Self {
            selected_indices,
            remaining_tokens: token_budget.saturating_sub(protected_tokens),
            max_messages,
        }
    }

    /// Admit eligible messages whole in rank order, skipping protected identities
    /// before applying the count cap just as the original user phase did.
    fn admit_ranked(&mut self, ranked: &[ScoredHistoryMessage<'_>]) {
        for candidate in ranked {
            if self.selected_indices.contains(&candidate.source_index) {
                continue;
            }
            if self.selected_indices.len() >= self.max_messages {
                break;
            }
            let estimated = candidate.message.estimate_tokens();
            if estimated <= self.remaining_tokens {
                self.selected_indices.push(candidate.source_index);
                self.remaining_tokens = self.remaining_tokens.saturating_sub(estimated);
            }
        }
    }

    /// Restore chronological order, then retain required results as one coherent
    /// protocol closure even when that closure exceeds the soft optional cap.
    fn into_coherent_history(mut self, history: &[Message]) -> Vec<Message> {
        self.selected_indices.sort_unstable();
        let selected = self
            .selected_indices
            .into_iter()
            .map(|index| (index, history[index].clone()))
            .collect::<Vec<_>>();
        coherence_tool_call_pairs(history, &selected)
            .into_iter()
            .map(|(_, message)| message)
            .collect()
    }
}

/// Keep retained tool-call turns internally consistent.
///
/// A `Tool` message references a tool call the model must have seen, and an
/// `Assistant` message that still carries `tool_calls` must be followed by the
/// results those calls produced. Sending either without its counterpart is
/// invalid: providers reject unmatched tool calls, and orphaned tool results
/// reference a call the model never observed. This pass:
///
/// - **Force-keeps** the `Tool` messages immediately following any retained
///   `Assistant` that carries `tool_calls`, so the model observes each call's
///   return value. A complete turn ends with its results, which survive even
///   if they push past the soft `max_messages` cap.
/// - **Drops** a retained `Tool` message whose calling `Assistant` (the message
///   directly before it in `history`) was *not* retained — an orphaned result
///   the model cannot reconcile.
///
/// Tool results that follow a plain `Assistant` (no `tool_calls`) are ordinary
/// turn output and are kept exactly as selected.
pub(in crate::compaction) fn coherence_tool_call_pairs(
    history: &[Message],
    selected: &[(usize, Message)],
) -> Vec<(usize, Message)> {
    let mut keep: std::collections::HashSet<usize> = selected.iter().map(|(index, _)| *index).collect();
    for (index, _) in selected.iter().filter(|(_, message)| introduces_tool_calls(message)) {
        keep.extend(
            history
                .iter()
                .enumerate()
                .skip(*index + 1)
                .take_while(|(_, message)| message.role == MessageRole::Tool)
                .map(|(result_index, _)| result_index),
        );
    }
    let mut indices = keep.iter().copied().collect::<Vec<_>>();
    indices.sort_unstable();
    indices
        .into_iter()
        .filter(|index| result_has_retained_origin(history, *index, &keep))
        .map(|index| (index, history[index].clone()))
        .collect()
}

/// Only nonempty assistant call metadata creates a tool-result dependency.
fn introduces_tool_calls(message: &Message) -> bool {
    message.role == MessageRole::Assistant && message.tool_calls.as_ref().is_some_and(|calls| !calls.is_empty())
}

/// Reject results of an unretained calling assistant, while preserving ordinary
/// messages and legacy results after plain assistants or at history boundaries.
fn result_has_retained_origin(history: &[Message], index: usize, keep: &std::collections::HashSet<usize>) -> bool {
    if history[index].role != MessageRole::Tool {
        return true;
    }
    let origin = history[..index]
        .iter()
        .enumerate()
        .rev()
        .find(|(_, message)| message.role != MessageRole::Tool);
    match origin {
        Some((origin_index, message)) if introduces_tool_calls(message) => keep.contains(&origin_index),
        _ => true,
    }
}

/// Score a message for importance-weighted retention during compaction.
///
/// Uses a weighted combination of content importance and recency:
/// - Messages containing errors, corrections, or tool results score higher
/// - Recent messages get a recency bonus
/// - Assistant messages with tool calls are moderately important
fn score_message(message: &Message, index: usize, total: usize) -> f64 {
    let importance = message_importance(message);
    // Recency bonus: linear from 0.0 (oldest) to 1.0 (newest).
    let recency = if total > 0 { index as f64 / total as f64 } else { 0.0 };
    importance + recency
}

/// Intrinsic retention weight, independent of conversation position. Error and
/// correction vocabulary retains precedence over the ordinary user weight.
fn message_importance(message: &Message) -> f64 {
    let content = message.content.as_text();
    let content_lower = content.to_lowercase();

    // Importance weight based on content signals.
    match message.role {
        MessageRole::User => {
            if contains_error_signal(&content_lower) {
                3.0
            } else if contains_correction_signal(&content_lower) {
                2.5
            } else {
                1.0
            }
        }
        MessageRole::Tool => {
            // Tool results contain factual data the model may need.
            2.0
        }
        MessageRole::Assistant => {
            if message.tool_calls.is_some() {
                // Assistant messages with tool calls show action taken.
                0.5
            } else {
                0.1
            }
        }
        MessageRole::System => 0.0,
    }
}

/// Check if content contains error or failure signals.
fn contains_error_signal(content: &str) -> bool {
    content.contains("error")
        || content.contains("failed")
        || content.contains("failure")
        || content.contains("panic")
        || content.contains("bug")
        || content.contains("broken")
        || content.contains("regression")
}

/// Check if content contains user correction signals.
fn contains_correction_signal(content: &str) -> bool {
    content.contains("no,")
        || content.contains("wrong")
        || content.contains("actually")
        || content.contains("fix")
        || content.contains("instead")
        || content.contains("should be")
        || content.contains("don't")
}

/// Whether a message is worth retaining during compaction.
fn is_retainable_message(message: &Message) -> bool {
    match message.role {
        MessageRole::User => !message.content.trim().is_empty(),
        MessageRole::Tool => !message.content.trim().is_empty(),
        MessageRole::Assistant => {
            // Retain assistant messages that contain tool calls (action history).
            message.tool_calls.is_some()
        }
        MessageRole::System => false,
    }
}
