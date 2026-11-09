//! ACP tool-call history recovery preserves durable evidence without replaying
//! uncertain effects. Before execution, callers stage incomplete result
//! placeholders beside each assistant request; normal completion replaces them
//! with terminal results. Legacy archives receive the same repair on load.
//!
//! A uniquely attributable late terminal result may be returned to its request
//! batch. Duplicate, conflicting, or orphaned evidence is ambiguous: recovery
//! fails closed, leaves the archive as the durable record, and never replays a
//! tool call automatically.

use super::super::types::ToolCallResult;
use diapason_core::core::threads::ThreadRuntimeHandle;
use diapason_core::llm::provider::{Message, MessageRole, ToolCall};
use diapason_core::utils::session_archive::SessionMessage;
use serde_json::json;
use std::collections::HashSet;

/// A message the repair inserted or relocated, with its position in the
/// repaired history.
///
/// The repair checkpoint must retain this exact message, and only its position
/// identifies it. A later batch may reuse the call id, so searching the history
/// for a matching request or result can select the wrong batch; two batches can
/// even carry byte-identical requests. Compaction only removes messages from
/// the front, so the position recorded here fixes the one persisted slot that
/// can still carry the repaired evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct RetainedEvidence {
    /// Index of the evidence message in the repaired history.
    pub(super) history_index: usize,
    /// The message the repair made durable at that position.
    pub(super) message: SessionMessage,
}

/// Counts placeholders inserted and results relocated while repairing an
/// archived tool-call history, plus any front-truncated tool-result prefix.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RecoveryReport {
    pub(super) repaired_calls: usize,
    pub(super) relocated_calls: usize,
    pub(super) truncated_prefix: usize,
    /// The assistant requests and result messages this repair inserted or
    /// relocated evidence for, in history order.
    ///
    /// The staged batch alone is not enough: a repair can touch an earlier
    /// batch, and compaction must not drop the evidence the repair just made
    /// durable. The message content, not only the call id, distinguishes the
    /// evidence from a later batch that reuses the id, and its recorded
    /// position distinguishes evidence that content cannot. A batch that was
    /// repaired also names its own request: a later batch reusing the id can
    /// satisfy the per-id checks by itself, so only the request's own position
    /// shows that the call it answers was retained.
    pub(super) evidence: Vec<RetainedEvidence>,
}

impl RecoveryReport {
    /// Whether recovery changed message order or content.
    ///
    /// A relocation-only repair inserts no placeholder but does reorder the
    /// history, so it must still be persisted and checkpointed.
    pub(super) fn history_changed(&self) -> bool {
        self.repaired_calls > 0 || self.relocated_calls > 0
    }
}

/// Signals that archived terminal results cannot be attributed safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AmbiguousToolResultHistory;

impl std::fmt::Display for AmbiguousToolResultHistory {
    /// Writes the fixed ambiguity description.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ambiguous tool-result attribution")
    }
}

impl std::error::Error for AmbiguousToolResultHistory {}

/// Appends an assistant tool request and one incomplete result for every call.
pub(super) fn stage_tool_calls(messages: &mut Vec<Message>, assistant: Message) -> Vec<String> {
    let calls = assistant.tool_calls.as_deref().unwrap_or_default().to_vec();
    let call_ids = calls.iter().map(|call| call.id.clone()).collect();
    messages.push(assistant);
    messages.extend(calls.iter().map(incomplete_tool_result));
    call_ids
}

/// Replaces staged result placeholders with terminal tool-call results.
pub(super) fn replace_tool_results(messages: &mut Vec<Message>, results: &[ToolCallResult]) -> usize {
    let mut finalized = 0;
    for result in results {
        // Reused ids across separate completed batches are accepted by this
        // module, so the newest matching Tool message is the current batch's
        // staged placeholder. Matching the first would overwrite an earlier,
        // already-terminal result and leave the new placeholder incomplete.
        if let Some(message) = messages.iter_mut().rev().find(|message| {
            message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(&result.tool_call_id)
        }) {
            *message = Message::tool_response(result.tool_call_id.clone(), result.llm_response.clone());
        } else {
            messages.push(Message::tool_response(result.tool_call_id.clone(), result.llm_response.clone()));
        }
        finalized += 1;
    }
    finalized
}

/// Stages tool calls while holding the canonical thread-history lock.
pub(super) fn stage_thread_tool_calls(thread: &ThreadRuntimeHandle, assistant: Message) -> Vec<String> {
    thread.mutate_messages(|messages| stage_tool_calls(messages, assistant))
}

/// Replaces staged results while holding the canonical thread-history lock.
pub(super) fn replace_thread_tool_results(thread: &ThreadRuntimeHandle, results: &[ToolCallResult]) -> usize {
    thread.mutate_messages(|messages| replace_tool_results(messages, results))
}

/// Returns the end index of the contiguous tool-result group starting at `start`.
///
/// Tool results follow their assistant request immediately, so the first
/// non-Tool message delimits the group. The caller recomputes the end after
/// relocation because relocating results changes the group's membership.
fn tool_result_group_end(messages: &[Message], start: usize) -> usize {
    let mut end = start;
    while messages.get(end).is_some_and(|message| message.role == MessageRole::Tool) {
        end += 1;
    }
    end
}

/// Collects the tool-call IDs completed inside the result group `start..end`.
///
/// IDs are owned so the caller can mutate the message vector after collecting
/// them.
fn completed_tool_call_ids(messages: &[Message], start: usize, end: usize) -> HashSet<String> {
    messages
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref().map(str::to_string))
        .collect()
}

/// Repairs missing terminal results without replaying any archived tool call.
pub(super) fn repair_unresolved_tool_calls(
    messages: &mut Vec<Message>,
) -> Result<RecoveryReport, AmbiguousToolResultHistory> {
    let mut repaired_calls = 0;
    let mut relocated_calls = 0;
    let mut evidence = Vec::new();
    let mut message_index = 0;

    // Archive snapshots are compacted from the front
    // (`trim_oldest_snapshot_entries` removes `snapshot.messages[0]`), so a
    // retained suffix can begin with tool results whose assistant request was
    // trimmed away. A complete archive never begins with a Tool message.
    let truncated_prefix = messages.iter().take_while(|message| message.role == MessageRole::Tool).count();

    while message_index < messages.len() {
        let Some(message) = messages.get(message_index) else {
            break;
        };
        let calls = message
            .tool_calls
            .as_deref()
            .filter(|_| message.role == MessageRole::Assistant)
            .unwrap_or_default()
            .to_vec();
        if calls.is_empty() {
            message_index += 1;
            continue;
        }

        let group_end = tool_result_group_end(messages, message_index + 1);

        let mut batch_evidence = Vec::new();
        let relocated_for_batch =
            relocate_unique_late_results(messages, message_index, group_end, &calls, &mut batch_evidence);
        relocated_calls += relocated_for_batch;
        let group_end = tool_result_group_end(messages, message_index + 1);
        let completed_ids = completed_tool_call_ids(messages, message_index + 1, group_end);

        let missing_calls = calls
            .iter()
            .filter(|call| !completed_ids.contains(call.id.as_str()))
            .collect::<Vec<_>>();
        let missing = missing_calls.into_iter().map(incomplete_tool_result).collect::<Vec<_>>();

        // The repaired batch's own request must survive with its evidence: a
        // later batch that reuses the id can request and answer it on its own,
        // so the per-id checks alone cannot prove this request was retained.
        // The request precedes every result this batch made durable, which is
        // the order the positional retention check requires.
        let repaired_batch = relocated_for_batch > 0 || !missing.is_empty();
        if repaired_batch && let Some(request) = messages.get(message_index) {
            evidence.push(RetainedEvidence {
                history_index: message_index,
                message: SessionMessage::from(request),
            });
        }
        evidence.append(&mut batch_evidence);
        repaired_calls += missing.len();
        // Each placeholder is inserted at a known offset, so the repair can
        // record its position without searching the history for it later.
        evidence.extend(missing.iter().enumerate().map(|(offset, placeholder)| RetainedEvidence {
            history_index: group_end + offset,
            message: SessionMessage::from(placeholder),
        }));
        let missing_count = missing.len();
        drop(messages.splice(group_end..group_end, missing));
        message_index = group_end + missing_count;
    }

    reject_ambiguous_tool_result_history(messages)?;
    // Every recorded entry owns a distinct history position, so the repair
    // cannot record the same slot twice and needs no de-duplication.
    Ok(RecoveryReport {
        repaired_calls,
        relocated_calls,
        truncated_prefix,
        evidence,
    })
}

/// Validates that one completed assistant batch has unique matching results.
fn validate_completed_tool_batch(
    messages: &[Message],
    message_index: &mut usize,
    calls: &[ToolCall],
) -> Result<(), AmbiguousToolResultHistory> {
    let call_ids = calls.iter().map(|call| call.id.as_str()).collect::<HashSet<_>>();
    if call_ids.len() != calls.len() {
        return Err(AmbiguousToolResultHistory);
    }

    *message_index += 1;
    let mut result_ids = HashSet::with_capacity(calls.len());
    while let Some(result) = messages.get(*message_index).filter(|result| result.role == MessageRole::Tool) {
        let Some(call_id) = result.tool_call_id.as_deref() else {
            return Err(AmbiguousToolResultHistory);
        };
        if !call_ids.contains(call_id) || !result_ids.insert(call_id) {
            return Err(AmbiguousToolResultHistory);
        }
        *message_index += 1;
    }
    Ok(())
}

/// Refuse to resume a history whose tool results cannot be mapped one-for-one
/// to the immediately preceding assistant batch. The archive remains the
/// durable record of the uncertain effect; guessing a canonical wire history
/// here could hide a result or cause a later continuation to use the wrong one.
fn reject_ambiguous_tool_result_history(messages: &[Message]) -> Result<(), AmbiguousToolResultHistory> {
    // A leading run of Tool results is the only shape front-truncation can
    // produce, and provider request builders already strip globally orphaned
    // tool blocks at serialization time. Rejecting it would make previously
    // resumable compacted archives unresumable, so the scan starts after that
    // run. Every later orphan, duplicate, mismatched, or out-of-batch Tool
    // result is still rejected; these leading messages are retained, not
    // dropped.
    let mut message_index = messages.iter().take_while(|message| message.role == MessageRole::Tool).count();
    while let Some(message) = messages.get(message_index) {
        if message.role != MessageRole::Assistant || message.tool_calls.as_deref().is_none_or(|calls| calls.is_empty())
        {
            if message.role == MessageRole::Tool {
                return Err(AmbiguousToolResultHistory);
            }
            message_index += 1;
            continue;
        }

        let calls = message.tool_calls.as_deref().unwrap_or_default();
        validate_completed_tool_batch(messages, &mut message_index, calls)?;
    }
    Ok(())
}

/// Moves uniquely attributable late results back to their request batch,
/// returning how many results were actually relocated.
fn relocate_unique_late_results(
    messages: &mut Vec<Message>,
    assistant_index: usize,
    contiguous_end: usize,
    calls: &[ToolCall],
    evidence: &mut Vec<RetainedEvidence>,
) -> usize {
    let mut relocated = 0;
    let mut result_end = contiguous_end;
    for call in calls {
        let call_id = call.id.as_str();
        let call_count = messages
            .iter()
            .filter(|message| message.role == MessageRole::Assistant)
            .flat_map(|message| message.tool_calls.iter().flatten())
            .filter(|candidate| candidate.id == call_id)
            .take(2)
            .count();
        if call_count != 1 {
            continue;
        }

        let matching_results = messages
            .iter()
            .enumerate()
            .filter(|(_, message)| {
                message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        let [result_index] = matching_results.as_slice() else {
            continue;
        };
        if *result_index < contiguous_end {
            continue;
        }

        let result = messages.remove(*result_index);
        messages.insert(result_end, result);
        if let Some(relocated_result) = messages.get(result_end) {
            evidence.push(RetainedEvidence {
                history_index: result_end,
                message: SessionMessage::from(relocated_result),
            });
        }
        result_end += 1;
        relocated += 1;
    }

    debug_assert!(
        messages
            .get(assistant_index)
            .is_some_and(|message| message.role == MessageRole::Assistant)
    );

    relocated
}

/// Creates the durable uncertain-effect placeholder for an incomplete call.
fn incomplete_tool_result(call: &ToolCall) -> Message {
    let tool_name = call.function.as_ref().map_or("unknown", |function| function.name.as_str());
    let content = json!({
        "status": "incomplete",
        "tool": tool_name,
        "message": "Tool execution did not reach a durable terminal result. Its side effects are uncertain.",
        "replayed": false,
        "state": "uncertain",
        "next_action": "Verify the current workspace state, then resubmit the tool call only if the requested work is still needed."
    })
    .to_string();
    Message::tool_response(call.id.clone(), content)
}

#[cfg(test)]
mod tests;
