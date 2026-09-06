use crate::llm::provider::Message;
use hashbrown::{HashMap, HashSet};
use std::time::Duration;
use vtcode_macros::StringNewtype;

// ============================================================================
// Context Manager: Call/Output Pairing Invariants (OpenAI Codex pattern)
// ============================================================================

/// Unique identifier for a tool call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, StringNewtype)]
pub struct ToolCallId(String);

/// Status of a tool execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStatus {
    Success,
    Failed,
    Canceled,
    Timeout,
}

impl OutputStatus {
    /// Convert to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
            Self::Timeout => "timeout",
        }
    }
}

/// Items that participate in call/output pairing for validation
#[derive(Debug, Clone)]
pub enum PairableHistoryItem {
    /// Tool call without output (yet)
    ToolCall { call_id: ToolCallId, tool_name: String },
    /// Tool output for a previous call
    ToolOutput { call_id: ToolCallId, status: OutputStatus },
}

/// Record of a missing output in conversation history
#[derive(Debug, Clone)]
pub struct MissingOutput {
    pub call_id: ToolCallId,
    pub tool_name: String,
}

/// Validation report for conversation history state
#[derive(Debug, Default, Clone)]
pub struct HistoryValidationReport {
    /// Tool calls without corresponding outputs
    pub missing_outputs: Vec<MissingOutput>,
    /// Outputs without corresponding calls (orphans)
    pub orphan_outputs: Vec<ToolCallId>,
}

impl HistoryValidationReport {
    /// Check if history is in a valid state
    pub fn is_valid(&self) -> bool {
        self.missing_outputs.is_empty() && self.orphan_outputs.is_empty()
    }

    /// Get a human-readable summary
    pub fn summary(&self) -> String {
        if self.is_valid() {
            "History invariants are valid".to_string()
        } else {
            format!("{} missing outputs, {} orphan outputs", self.missing_outputs.len(), self.orphan_outputs.len())
        }
    }

    /// Filter out missing outputs that correspond to pending actions.
    ///
    /// A "missing output" is expected when the corresponding tool call is still
    /// in flight (pending action). Removing these from the report prevents
    /// false positives during crash recovery that would insert synthetic
    /// outputs for tools that are legitimately still running.
    pub fn exclude_pending(&mut self, is_pending: impl Fn(&str) -> bool) {
        self.missing_outputs.retain(|m| !is_pending(m.call_id.as_str()));
    }
}

#[cfg(test)]
#[inline]
pub(crate) fn record_turn_duration(
    turn_durations: &mut Vec<u128>,
    turn_total_ms: &mut u128,
    turn_max_ms: &mut u128,
    turn_count: &mut usize,
    recorded: &mut bool,
    start: &std::time::Instant,
) {
    if !*recorded {
        let duration_ms = start.elapsed().as_millis();
        turn_durations.push(duration_ms);
        *turn_total_ms += duration_ms;
        if duration_ms > *turn_max_ms {
            *turn_max_ms = duration_ms;
        }
        *turn_count += 1;
        *recorded = true;
    }
}

/// API failure tracking for exponential backoff
pub struct ApiFailureTracker {
    pub consecutive_failures: u32,
    pub last_failure: Option<std::time::Instant>,
}

impl Default for ApiFailureTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiFailureTracker {
    pub fn new() -> Self {
        Self { consecutive_failures: 0, last_failure: None }
    }

    pub fn record_failure(&mut self) {
        self.consecutive_failures += 1;
        self.last_failure = Some(std::time::Instant::now());
    }

    pub fn reset(&mut self) {
        self.consecutive_failures = 0;
        self.last_failure = None;
    }

    pub fn should_circuit_break(&self) -> bool {
        self.consecutive_failures >= 3
    }

    pub fn backoff_duration(&self) -> Duration {
        let base_ms = 1000;
        let max_ms = 30000;
        let backoff_ms = base_ms * 2_u64.pow(self.consecutive_failures.saturating_sub(1));
        Duration::from_millis(backoff_ms.min(max_ms))
    }
}

pub fn summarize_list(items: &[String]) -> String {
    const MAX_ITEMS: usize = 5;
    if items.is_empty() {
        return "none".into();
    }
    let shown: Vec<&str> = items.iter().take(MAX_ITEMS).map(|s| s.as_str()).collect();
    if items.len() > MAX_ITEMS {
        format!("{} [+{} more]", shown.join(", "), items.len() - MAX_ITEMS)
    } else {
        shown.join(", ")
    }
}

// ============================================================================
// Standalone History Invariant Functions
// ============================================================================

/// Validate that conversation history maintains call/output invariants.
pub fn validate_history_invariants(messages: &[Message]) -> HistoryValidationReport {
    let mut call_map: HashMap<String, String> = HashMap::new();
    let mut output_ids: HashSet<String> = HashSet::new();

    // Scan messages to find tool calls and responses
    for msg in messages {
        // Tool calls: assistant messages with tool_calls field
        if let Some(tool_calls) = &msg.tool_calls {
            for tool_call in tool_calls {
                call_map.insert(tool_call.id.clone(), msg.role.to_string());
            }
        }

        // Tool responses: messages with tool_call_id set
        if let Some(tool_call_id) = &msg.tool_call_id {
            output_ids.insert(tool_call_id.clone());
        }
    }

    // Find missing outputs (calls without corresponding responses)
    let missing_outputs: Vec<_> = call_map
        .keys()
        .filter(|call_id| !output_ids.contains(*call_id))
        .map(|call_id| MissingOutput {
            call_id: ToolCallId::new(call_id.clone()),
            tool_name: "unknown".to_string(),
        })
        .collect();

    // Find orphan outputs (responses without matching calls)
    let orphan_outputs: Vec<_> = output_ids
        .iter()
        .filter(|output_id| !call_map.contains_key(*output_id))
        .map(|output_id| ToolCallId::new(output_id.clone()))
        .collect();

    HistoryValidationReport { missing_outputs, orphan_outputs }
}

const REQUEST_HISTORY_CANCELLATION_RESULT: &str =
    "canceled: no tool result was recorded; this bounded placeholder preserves the tool-call protocol.";

#[derive(Debug)]
struct RequestToolBatch {
    assistant_index: usize,
    call_ids: Vec<String>,
    matched_calls: Vec<bool>,
    result_indices: Vec<usize>,
}

#[derive(Debug, Default)]
struct RequestHistoryAnalysis {
    batches: Vec<RequestToolBatch>,
    batch_by_assistant_index: HashMap<usize, usize>,
    result_batch_by_index: HashMap<usize, usize>,
    invalid_result_indices: HashSet<usize>,
}

impl RequestHistoryAnalysis {
    fn needs_repair(&self) -> bool {
        if !self.invalid_result_indices.is_empty() {
            return true;
        }

        self.batches.iter().any(|batch| {
            batch.matched_calls.iter().any(|matched| !matched)
                || batch
                    .result_indices
                    .iter()
                    .enumerate()
                    .any(|(offset, &result_index)| result_index != batch.assistant_index + offset + 1)
        })
    }
}

fn analyse_request_history(messages: &[Message]) -> RequestHistoryAnalysis {
    let mut analysis = RequestHistoryAnalysis::default();
    let mut calls_by_id: HashMap<String, Vec<(usize, usize)>> = HashMap::new();

    for (message_index, message) in messages.iter().enumerate() {
        if message.role != crate::llm::provider::MessageRole::Assistant {
            continue;
        }

        let Some(tool_calls) = message.tool_calls.as_ref().filter(|calls| !calls.is_empty()) else {
            continue;
        };

        let batch_index = analysis.batches.len();
        let call_ids = tool_calls.iter().map(|call| call.id.clone()).collect::<Vec<_>>();
        let matched_calls = vec![false; call_ids.len()];
        analysis.batch_by_assistant_index.insert(message_index, batch_index);
        analysis.batches.push(RequestToolBatch {
            assistant_index: message_index,
            call_ids,
            matched_calls,
            result_indices: Vec::new(),
        });

        for (call_index, call) in tool_calls.iter().enumerate() {
            calls_by_id.entry(call.id.clone()).or_default().push((batch_index, call_index));
        }
    }

    for (message_index, message) in messages.iter().enumerate() {
        let has_tool_call_id = message.tool_call_id.is_some();
        let is_tool_result = message.role == crate::llm::provider::MessageRole::Tool;
        if !is_tool_result && !has_tool_call_id {
            continue;
        }

        let Some(tool_call_id) = message.tool_call_id.as_deref().filter(|id| !id.is_empty()) else {
            analysis.invalid_result_indices.insert(message_index);
            continue;
        };

        if !is_tool_result {
            analysis.invalid_result_indices.insert(message_index);
            continue;
        }

        let Some(candidates) = calls_by_id.get_mut(tool_call_id) else {
            analysis.invalid_result_indices.insert(message_index);
            continue;
        };

        let Some(&(batch_index, call_index)) = candidates.iter().rev().find(|&&(batch_index, call_index)| {
            analysis.batches[batch_index].assistant_index < message_index
                && !analysis.batches[batch_index].matched_calls[call_index]
        }) else {
            // This includes results that arrived before their call and duplicate
            // results after the call has already been matched.
            analysis.invalid_result_indices.insert(message_index);
            continue;
        };

        analysis.batches[batch_index].matched_calls[call_index] = true;
        analysis.batches[batch_index].result_indices.push(message_index);
        analysis.result_batch_by_index.insert(message_index, batch_index);
    }

    analysis
}

/// Return whether the provider-facing message view needs tool-history repair.
///
/// This deliberately analyzes only the request view. Durable session history
/// is repaired by its own crash-recovery path and is never changed here.
pub fn request_history_needs_normalization(messages: &[Message]) -> bool {
    analyse_request_history(messages).needs_repair()
}

/// Rebuild a provider-facing message view with each assistant tool-call batch
/// followed immediately by its matching results.
///
/// The rebuild is request-scoped and idempotent: orphaned, causally early, and
/// duplicate results are omitted; missing calls receive a bounded cancellation
/// result; and messages interleaved between a call batch and its results move
/// after the complete batch. The input slice is never mutated.
pub fn normalize_history_for_request(messages: &[Message]) -> Vec<Message> {
    let analysis = analyse_request_history(messages);
    if !analysis.needs_repair() {
        return messages.to_vec();
    }

    let mut normalized = Vec::with_capacity(messages.len());
    for (message_index, message) in messages.iter().enumerate() {
        if analysis.invalid_result_indices.contains(&message_index)
            || analysis.result_batch_by_index.contains_key(&message_index)
        {
            continue;
        }

        let Some(&batch_index) = analysis.batch_by_assistant_index.get(&message_index) else {
            normalized.push(message.clone());
            continue;
        };

        let batch = &analysis.batches[batch_index];
        normalized.push(message.clone());
        for &result_index in &batch.result_indices {
            normalized.push(messages[result_index].clone());
        }
        for (call_id, matched) in batch.call_ids.iter().zip(&batch.matched_calls) {
            if !matched {
                normalized
                    .push(Message::tool_response(call_id.clone(), REQUEST_HISTORY_CANCELLATION_RESULT.to_owned()));
            }
        }
    }

    normalized
}

/// Find a split point that keeps tool-call outputs paired with their calls.
pub fn safe_history_split_point(messages: &[Message], conversation_len: usize, preferred_split_at: usize) -> usize {
    if preferred_split_at == 0 || preferred_split_at >= conversation_len {
        return preferred_split_at;
    }

    let mut call_indices: HashMap<&str, usize> = HashMap::new();
    for (i, msg) in messages.iter().enumerate() {
        if let Some(tool_calls) = &msg.tool_calls {
            for call in tool_calls {
                call_indices.insert(&call.id, i);
            }
        }
    }

    let mut safe_split_at = preferred_split_at;
    loop {
        if safe_split_at == 0 {
            break;
        }

        let has_orphan = ((safe_split_at + 1)..messages.len()).any(|i| {
            messages
                .get(i)
                .and_then(|msg| msg.tool_call_id.as_ref())
                .and_then(|id| call_indices.get(id.as_str()))
                .is_some_and(|&call_idx| call_idx <= safe_split_at)
        });

        if !has_orphan {
            break;
        }

        safe_split_at -= 1;
    }

    safe_split_at
}

/// Ensure all tool calls have corresponding outputs in the message list.
pub fn ensure_call_outputs_present(messages: &mut Vec<Message>) {
    let report = validate_history_invariants(messages);

    // Create synthetic outputs for missing calls in reverse order to avoid index shifting
    for missing in report.missing_outputs.iter().rev() {
        let synthetic_message = Message::tool_response(
            missing.call_id.as_str().to_string(),
            "canceled: Tool execution was interrupted. This synthetic output was created \
             during history normalization to maintain conversation invariants."
                .to_string(),
        );

        tracing::warn!("Creating synthetic output for call {} due to missing execution result", missing.call_id);

        // Find the position to insert: right after the corresponding call
        let insert_pos = messages
            .iter()
            .position(|msg| {
                msg.tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| call.id == missing.call_id.as_str()))
            })
            .map(|pos| pos + 1);

        if let Some(pos) = insert_pos {
            messages.insert(pos, synthetic_message);
        } else {
            // If we can't find the call, just append the synthetic output
            messages.push(synthetic_message);
        }
    }
}

/// Remove outputs without corresponding calls (orphaned outputs) from the message list.
pub fn remove_orphan_outputs(messages: &mut Vec<Message>) {
    let report = validate_history_invariants(messages);

    if report.orphan_outputs.is_empty() {
        return;
    }

    let orphan_ids: HashSet<String> = report.orphan_outputs.iter().map(|id| id.as_str().to_string()).collect();

    let initial_len = messages.len();

    // Retain only messages that either:
    // - Don't have a tool_call_id (not a tool response)
    // - Have a tool_call_id that matches an existing call
    messages.retain(|msg| {
        if let Some(tool_call_id) = msg.tool_call_id.as_ref()
            && orphan_ids.contains(tool_call_id)
        {
            tracing::warn!("Removing orphan output for call {}", tool_call_id);
            return false;
        }
        true
    });

    if messages.len() != initial_len {
        tracing::info!("Removed {} orphan outputs", initial_len - messages.len());
    }
}

/// Normalize history to enforce call/output pairing invariants.
pub fn normalize_history(messages: &mut Vec<Message>) {
    ensure_call_outputs_present(messages);
    remove_orphan_outputs(messages);

    // Log if issues were found
    let report = validate_history_invariants(messages);
    if !report.is_valid() {
        tracing::warn!("History validation: {}", report.summary());
    } else {
        tracing::debug!("History normalized successfully");
    }
}

/// Recover from crashed or interrupted session by fixing history invariants.
pub fn recover_history_from_crash(messages: &mut Vec<Message>) {
    let report = validate_history_invariants(messages);

    if !report.missing_outputs.is_empty() {
        tracing::warn!("Found {} missing outputs during recovery", report.missing_outputs.len());
        ensure_call_outputs_present(messages);
    }

    if !report.orphan_outputs.is_empty() {
        tracing::warn!("Found {} orphan outputs during recovery", report.orphan_outputs.len());
        remove_orphan_outputs(messages);
    }

    if report.is_valid() {
        tracing::debug!("History invariants are valid");
    }
}

// ============================================================================
// Tests: Context Manager - Call/Output Pairing Invariants
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::provider::{Message, MessageRole};
    /// Helper: Create test messages
    fn make_tool_call(call_id: &str, tool_name: &str) -> Message {
        Message::assistant_with_tools(
            "".to_string(),
            vec![crate::llm::provider::ToolCall::function(
                call_id.to_string(),
                tool_name.to_string(),
                "{}".to_string(),
            )],
        )
    }

    fn make_tool_response(call_id: &str, content: &str) -> Message {
        Message::tool_response(call_id.to_string(), content.to_string())
    }

    /// Test: Valid history with matched calls and outputs
    #[test]
    fn test_validate_history_valid_matched_pairs() {
        let mut messages = vec![
            make_tool_call("call_1", "list_files"),
            make_tool_response("call_1", "file1.rs\nfile2.rs"),
        ];

        let report = validate_history_invariants(&messages);
        assert!(report.is_valid(), "Valid paired call/output should pass");
        assert!(report.missing_outputs.is_empty());
        assert!(report.orphan_outputs.is_empty());

        // Normalize should be a no-op
        normalize_history(&mut messages);
        assert_eq!(messages.len(), 2);
    }

    /// Test: Missing output (tool call without response)
    #[test]
    fn test_validate_history_missing_output() {
        let messages = vec![make_tool_call("call_1", "list_files")];

        let report = validate_history_invariants(&messages);
        assert!(!report.is_valid());
        assert_eq!(report.missing_outputs.len(), 1);
        assert_eq!(report.missing_outputs[0].call_id.as_str(), "call_1");
        assert!(report.orphan_outputs.is_empty());
    }

    /// Test: Orphan output (response without corresponding call)
    #[test]
    fn test_validate_history_orphan_output() {
        let messages = vec![make_tool_response("orphan_call", "Some result")];

        let report = validate_history_invariants(&messages);
        assert!(!report.is_valid());
        assert!(report.missing_outputs.is_empty());
        assert_eq!(report.orphan_outputs.len(), 1);
        assert_eq!(report.orphan_outputs[0].as_str(), "orphan_call");
    }

    /// Test: ensure_call_outputs_present creates synthetic outputs
    #[test]
    fn test_ensure_call_outputs_present() {
        let mut messages = vec![make_tool_call("call_1", "list_files")];
        let initial_len = messages.len();

        ensure_call_outputs_present(&mut messages);

        assert_eq!(messages.len(), initial_len + 1);
        let last_msg = &messages[initial_len];
        assert_eq!(last_msg.tool_call_id, Some("call_1".to_string()));
        assert!(last_msg.content.as_text().contains("canceled"));

        let report = validate_history_invariants(&messages);
        assert!(report.is_valid());
    }

    /// Test: remove_orphan_outputs filters out orphaned responses
    #[test]
    fn test_remove_orphan_outputs() {
        let mut messages = vec![
            make_tool_call("call_1", "list_files"),
            make_tool_response("call_1", "valid result"),
            make_tool_response("orphan_call", "orphan result"),
        ];

        let initial_len = messages.len();
        remove_orphan_outputs(&mut messages);

        assert_eq!(messages.len(), initial_len - 1);
        assert!(
            messages
                .iter()
                .any(|msg| msg.tool_call_id.as_ref().is_some_and(|id| id == "call_1"))
        );
        assert!(
            !messages
                .iter()
                .any(|msg| { msg.tool_call_id.as_ref().is_some_and(|id| id == "orphan_call") })
        );

        let report = validate_history_invariants(&messages);
        assert!(report.is_valid());
    }

    /// Test: normalize() applies both fixes (synthetic output + orphan removal)
    #[test]
    fn test_normalize_combined_fixes() {
        let mut messages = vec![
            make_tool_call("call_1", "read_file"),
            make_tool_call("call_2", "write_file"),
            make_tool_response("call_2", "written"),
            make_tool_response("orphan", "orphan result"),
        ];

        normalize_history(&mut messages);

        let report = validate_history_invariants(&messages);
        assert!(report.is_valid());
        assert!(
            messages
                .iter()
                .any(|msg| msg.tool_call_id.as_ref().is_some_and(|id| id == "call_1"))
        );
        assert!(
            !messages
                .iter()
                .any(|msg| msg.tool_call_id.as_ref().is_some_and(|id| id == "orphan"))
        );
    }

    #[test]
    fn request_normalization_groups_split_results_after_intervening_messages() {
        let messages = vec![
            Message::assistant_with_tools(
                "".to_string(),
                vec![
                    crate::llm::provider::ToolCall::function(
                        "call_1".to_string(),
                        "read_file".to_string(),
                        "{}".to_string(),
                    ),
                    crate::llm::provider::ToolCall::function(
                        "call_2".to_string(),
                        "read_file".to_string(),
                        "{}".to_string(),
                    ),
                ],
            ),
            Message::system("intervening system note".to_string()),
            make_tool_response("call_2", "result two"),
            Message::user("intervening user note".to_string()),
            make_tool_response("call_1", "result one"),
            Message::assistant("done".to_string()),
        ];

        let normalized = normalize_history_for_request(&messages);

        assert_eq!(normalized[0].role, MessageRole::Assistant);
        assert_eq!(normalized[1].tool_call_id.as_deref(), Some("call_2"));
        assert_eq!(normalized[1].content.as_text(), "result two");
        assert_eq!(normalized[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(normalized[2].content.as_text(), "result one");
        assert_eq!(normalized[3].role, MessageRole::System);
        assert_eq!(normalized[4].role, MessageRole::User);
        assert_eq!(normalized[5].role, MessageRole::Assistant);
    }

    #[test]
    fn request_normalization_drops_early_orphan_duplicate_and_synthesizes_missing_results() {
        let messages = vec![
            make_tool_response("call_1", "causally early"),
            make_tool_response("orphan", "orphaned"),
            make_tool_call("call_1", "read_file"),
            make_tool_call("call_2", "read_file"),
            make_tool_response("call_1", "valid"),
            make_tool_response("call_1", "duplicate"),
        ];

        let normalized = normalize_history_for_request(&messages);

        assert_eq!(normalized.len(), 4);
        assert_eq!(normalized[0].role, MessageRole::Assistant);
        assert_eq!(normalized[0].tool_calls.as_ref().map(Vec::len), Some(1));
        assert_eq!(normalized[1].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(normalized[1].content.as_text(), "valid");
        assert_eq!(normalized[2].role, MessageRole::Assistant);
        assert_eq!(normalized[3].tool_call_id.as_deref(), Some("call_2"));
        assert!(normalized[3].content.as_text().contains("bounded placeholder"));
    }

    #[test]
    fn request_normalization_is_idempotent_and_does_not_mutate_durable_history() {
        let messages = vec![
            make_tool_call("call_1", "read_file"),
            Message::system("intervening".to_string()),
            make_tool_response("call_1", "result"),
        ];
        let durable_before = messages.clone();

        assert!(request_history_needs_normalization(&messages));
        let normalized = normalize_history_for_request(&messages);
        assert!(!request_history_needs_normalization(&normalized));
        assert_eq!(normalize_history_for_request(&normalized), normalized);
        assert_eq!(messages, durable_before);
    }

    /// Test: recover_history_from_crash handles both missing and orphan outputs
    #[test]
    fn test_recover_from_crash() {
        let mut messages = vec![
            make_tool_call("crashed_call", "dangerous_op"),
            make_tool_response("old_call", "stale result"),
        ];

        recover_history_from_crash(&mut messages);

        let report = validate_history_invariants(&messages);
        assert!(report.is_valid());
        assert!(
            messages
                .iter()
                .any(|msg| { msg.tool_call_id.as_ref().is_some_and(|id| id == "crashed_call") })
        );
        assert!(
            !messages
                .iter()
                .any(|msg| msg.tool_call_id.as_ref().is_some_and(|id| id == "old_call"))
        );
    }

    /// Test: HistoryValidationReport summary messages
    #[test]
    fn test_validation_report_summary() {
        let valid = HistoryValidationReport::default();
        assert_eq!(valid.summary(), "History invariants are valid");
        assert!(valid.is_valid());

        let invalid = HistoryValidationReport {
            missing_outputs: vec![
                MissingOutput {
                    call_id: ToolCallId::new("call_1"),
                    tool_name: "tool_a".into(),
                },
                MissingOutput {
                    call_id: ToolCallId::new("call_2"),
                    tool_name: "tool_b".into(),
                },
            ],
            orphan_outputs: vec![ToolCallId::new("orphan_1")],
        };
        assert_eq!(invalid.summary(), "2 missing outputs, 1 orphan outputs");
        assert!(!invalid.is_valid());
    }

    /// Test: Multiple tool calls with selective missing outputs
    #[test]
    fn test_multiple_calls_partial_outputs() {
        let _messages: Vec<Message> = (1..=3)
            .flat_map(|i| {
                vec![
                    make_tool_call(&format!("call_{i}"), &format!("tool_{i}")),
                    if i != 2 {
                        make_tool_response(&format!("call_{i}"), &format!("result_{i}"))
                    } else {
                        // Simulate a gap: we don't add a response for call_2 here directly,
                        // but we need to build messages differently.
                        // Instead, build manually below.
                        Message::tool_response("placeholder".into(), "".into())
                    },
                ]
            })
            .collect();
        // Redo: explicit construction
        let mut messages = vec![
            make_tool_call("call_1", "tool_1"),
            make_tool_response("call_1", "result_1"),
            make_tool_call("call_2", "tool_2"),
            make_tool_call("call_3", "tool_3"),
            make_tool_response("call_3", "result_3"),
        ];

        let report = validate_history_invariants(&messages);
        assert!(!report.is_valid());
        assert_eq!(report.missing_outputs.len(), 1);
        assert_eq!(report.missing_outputs[0].call_id.as_str(), "call_2");

        normalize_history(&mut messages);
        assert!(validate_history_invariants(&messages).is_valid());
    }

    /// Test: OutputStatus enum conversion
    #[test]
    fn test_output_status_as_str() {
        assert_eq!(OutputStatus::Success.as_str(), "success");
        assert_eq!(OutputStatus::Failed.as_str(), "failed");
        assert_eq!(OutputStatus::Canceled.as_str(), "canceled");
        assert_eq!(OutputStatus::Timeout.as_str(), "timeout");
    }

    /// Test: find_safe_split_point maintains call/output pairs
    #[test]
    fn test_find_safe_split_point() {
        let messages = vec![
            Message::user("User 1".into()),           // 0
            make_tool_call("call_a", "tool_a"),       // 1
            make_tool_response("call_a", "Result A"), // 2
            make_tool_call("call_b", "tool_b"),       // 3
            make_tool_response("call_b", "Result B"), // 4
        ];
        let conversation_len = 5;

        // Split at 3 means keeping 3,4. But response at 2 needs call at 1 -> must split at 2.
        let safe = safe_history_split_point(&messages, conversation_len, 3);
        assert_eq!(safe, 2, "Should move split to include Call A");

        // Split at 4 is safe: call_b (3) and response_b (4) are both kept.
        let safe2 = safe_history_split_point(&messages, conversation_len, 4);
        assert_eq!(safe2, 4, "Should stay at 4 as it is safe");
    }

    #[test]
    fn test_summarize_list_formatting() {
        assert_eq!(summarize_list(&[]), "none");
        assert_eq!(summarize_list(&["a".into()]), "a");
        assert_eq!(summarize_list(&["a".into(), "b".into()]), "a, b");
        let many: Vec<String> = (1..=7).map(|i| format!("item{i}")).collect();
        let result = summarize_list(&many);
        assert!(result.contains("item1, item2, item3, item4, item5"));
        assert!(result.contains("[+2 more]"));
    }
}
