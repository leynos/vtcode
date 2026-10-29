//! Identify complete, user-anchored continuity suffixes and tool batches.
//!
//! Parallel results match each declared ID once, regardless of result order;
//! incomplete or malformed batches roll back to their originating assistant.
//! Legacy results without persisted call metadata retain their existing behavior.
//! Previewing oversized suffixes is delegated to `bounded_history`.

use super::CONTINUITY_TAIL_TARGET_TOKENS;
use super::bounded_history::bounded_protocol_group;
use crate::llm::provider::{Message, MessageRole};
use std::ops::Range;
use vtcode_commons::llm::ToolCall;

/// Source span selected for continuity, with oversized latest groups explicitly
/// distinguished from ordinary verbatim suffixes.
pub(super) struct ContinuityTailSelection {
    pub(super) range: Range<usize>,
    pub(super) oversized: bool,
}

/// Return the newest complete user-anchored protocol groups that fit the fixed
/// continuity budget. Only an oversized latest group receives bounded previews.
pub(super) fn continuity_tail(history: &[Message]) -> Vec<Message> {
    let Some(selection) = continuity_tail_selection(history) else {
        return Vec::new();
    };
    let group = &history[selection.range];
    if selection.oversized {
        bounded_protocol_group(group, CONTINUITY_TAIL_TARGET_TOKENS)
    } else {
        group.to_vec()
    }
}

/// Find source user groups, trim only the latest group's invalid protocol
/// suffix, then admit one contiguous complete suffix under the fixed budget.
pub(super) fn continuity_tail_selection(history: &[Message]) -> Option<ContinuityTailSelection> {
    let groups = user_group_ranges(history);
    let latest = groups.last()?;
    let latest_range = latest_complete_group_range(history, latest);
    Some(select_complete_suffix(history, &groups[..groups.len() - 1], latest_range))
}

/// Partition source user turns without including pre-user system history.
/// Each group ends at the next user turn or at the end of source history.
fn user_group_ranges(history: &[Message]) -> Vec<Range<usize>> {
    let mut groups: Vec<Range<usize>> = Vec::new();
    for (index, message) in history.iter().enumerate() {
        if message.role == MessageRole::User {
            if let Some(previous) = groups.last_mut() {
                previous.end = index;
            }
            groups.push(index..history.len());
        }
    }
    groups
}

/// Keep the latest user anchor while rolling back pending or malformed calls
/// to their originating assistant. User-anchored groups always retain that user.
fn latest_complete_group_range(history: &[Message], latest: &Range<usize>) -> Range<usize> {
    let prefix_len = complete_protocol_group_prefix(&history[latest.clone()]);
    latest.start..latest.start.saturating_add(prefix_len)
}

/// Start with the latest valid group and extend backward only across complete
/// groups that fit. An oversized latest group stands alone; an older oversized
/// or incomplete group stops admission without receiving a preview.
fn select_complete_suffix(
    history: &[Message],
    earlier_groups: &[Range<usize>],
    latest_range: Range<usize>,
) -> ContinuityTailSelection {
    let latest_tokens = history[latest_range.clone()]
        .iter()
        .map(Message::estimate_tokens)
        .sum::<usize>();
    let mut selection = ContinuityTailSelection {
        range: latest_range,
        oversized: latest_tokens > CONTINUITY_TAIL_TARGET_TOKENS,
    };
    if selection.oversized {
        return selection;
    }
    let mut remaining_tokens = CONTINUITY_TAIL_TARGET_TOKENS.saturating_sub(latest_tokens);
    for range in earlier_groups.iter().rev() {
        let group = &history[range.clone()];
        if !protocol_group_is_complete(group) {
            break;
        }
        let tokens = group.iter().map(Message::estimate_tokens).sum::<usize>();
        if tokens > remaining_tokens {
            break;
        }
        selection.range.start = range.start;
        remaining_tokens -= tokens;
    }
    selection
}

fn protocol_group_is_complete(group: &[Message]) -> bool {
    complete_protocol_group_prefix(group) == group.len()
}

/// Return the length of the valid protocol prefix in a user-anchored group.
/// When a tool call is still pending, the prefix ends before the assistant
/// message that introduced it. This handles parallel tool calls and prevents
/// a partially answered group from entering the continuity tail.
fn complete_protocol_group_prefix(group: &[Message]) -> usize {
    if group.first().is_none_or(|message| message.role != MessageRole::User) {
        return 0;
    }
    complete_protocol_prefix(group)
}

/// Return a valid prefix independently of whether history starts at a user turn.
/// Reject malformed call IDs and incomplete/mismatched results at the originating
/// assistant, while accepting legacy results when no batch is pending.
pub(super) fn complete_protocol_prefix(group: &[Message]) -> usize {
    let mut pending: Option<PendingToolBatch<'_>> = None;

    for (index, message) in group.iter().enumerate() {
        if let Some(batch) = pending.as_mut() {
            if !batch.consume_result(message) {
                return batch.origin;
            }
            if batch.call_ids.is_empty() {
                pending = None;
            }
            continue;
        }

        // Older persisted histories may contain a tool result without the
        // assistant call metadata that introduced it. Preserve that legacy
        // pair rather than discarding an otherwise complete protocol group.
        if message.role != MessageRole::Assistant {
            continue;
        }
        let Some(tool_calls) = message.tool_calls.as_ref().filter(|calls| !calls.is_empty()) else {
            continue;
        };
        let Some(batch) = PendingToolBatch::new(index, tool_calls) else {
            return index;
        };
        pending = Some(batch);
    }

    pending.map_or(group.len(), |batch| batch.origin)
}

/// An in-flight assistant batch owns both the rollback position and its
/// unmatched IDs. Results may arrive in any order, but each ID matches once.
struct PendingToolBatch<'a> {
    origin: usize,
    call_ids: Vec<&'a str>,
}

impl<'a> PendingToolBatch<'a> {
    /// Validate nonempty unique call IDs before a batch can become pending.
    /// The caller supplies a nonempty batch from an assistant message.
    fn new(origin: usize, tool_calls: &'a [ToolCall]) -> Option<Self> {
        let mut call_ids = Vec::with_capacity(tool_calls.len());
        for call in tool_calls {
            let id = call.id.as_str();
            if id.is_empty() || call_ids.contains(&id) {
                return None;
            }
            call_ids.push(id);
        }
        Some(Self { origin, call_ids })
    }

    /// Consume exactly one matching result, permitting out-of-order completion.
    /// Reject other roles, absent IDs and already consumed or unknown IDs.
    fn consume_result(&mut self, message: &Message) -> bool {
        if message.role != MessageRole::Tool {
            return false;
        }
        let Some(tool_call_id) = message.tool_call_id.as_deref() else {
            return false;
        };
        let Some(pending_index) = self.call_ids.iter().position(|pending_id| *pending_id == tool_call_id) else {
            return false;
        };
        self.call_ids.swap_remove(pending_index);
        true
    }
}

/// Separate optional prefix retention from the selected complete continuity
/// suffix. An owned suffix may contain bounded assistant/result previews.
pub(super) fn split_continuity_history(history: &[Message]) -> (&[Message], Vec<Message>) {
    let Some(selection) = continuity_tail_selection(history) else {
        return (history, Vec::new());
    };
    (&history[..selection.range.start], continuity_tail(history))
}
