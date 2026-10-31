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
use serde_json::json;
use std::collections::HashSet;
use vtcode_core::core::threads::ThreadRuntimeHandle;
use vtcode_core::llm::provider::{Message, MessageRole, ToolCall};

/// Counts placeholders inserted and results relocated while repairing an
/// archived tool-call history, plus any front-truncated tool-result prefix.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct RecoveryReport {
    pub(super) repaired_calls: usize,
    pub(super) relocated_calls: usize,
    pub(super) truncated_prefix: usize,
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

/// Repairs missing terminal results without replaying any archived tool call.
pub(super) fn repair_unresolved_tool_calls(
    messages: &mut Vec<Message>,
) -> Result<RecoveryReport, AmbiguousToolResultHistory> {
    let mut repaired_calls = 0;
    let mut relocated_calls = 0;
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

        let mut group_end = message_index + 1;
        while messages.get(group_end).is_some_and(|message| message.role == MessageRole::Tool) {
            group_end += 1;
        }

        relocated_calls += relocate_unique_late_results(messages, message_index, group_end, &calls);
        group_end = message_index + 1;
        let mut completed_ids = HashSet::with_capacity(calls.len());
        while let Some(message) = messages.get(group_end).filter(|message| message.role == MessageRole::Tool) {
            if let Some(call_id) = message.tool_call_id.as_deref() {
                let _ = completed_ids.insert(call_id.to_string());
            }
            group_end += 1;
        }

        let missing = calls
            .iter()
            .filter(|call| !completed_ids.contains(call.id.as_str()))
            .map(incomplete_tool_result)
            .collect::<Vec<_>>();
        repaired_calls += missing.len();
        let missing_count = missing.len();
        drop(messages.splice(group_end..group_end, missing));
        message_index = group_end + missing_count;
    }

    reject_ambiguous_tool_result_history(messages)?;
    Ok(RecoveryReport { repaired_calls, relocated_calls, truncated_prefix })
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
