//! Validates the parent `tool_recovery` module's history repair.
//!
//! The cases cover write-ahead staging of incomplete placeholders, their
//! replacement by terminal results, repair of legacy archives that lack
//! results, rejection of ambiguous attribution, and idempotence when recovery
//! runs over already-repaired history.

use super::*;
use proptest::collection::btree_map;
use proptest::prelude::*;
use std::collections::BTreeSet;
use vtcode_core::llm::provider::MessageContent;
use vtcode_safety::audit_log::ToolAuditStatus;

fn call(id: &str) -> ToolCall {
    ToolCall::function(id.to_string(), "apply_patch".to_string(), "{}".to_string())
}

fn assistant(calls: impl IntoIterator<Item = ToolCall>) -> Message {
    Message::assistant_with_tools(String::new(), calls.into_iter().collect())
}

fn tool_result(id: &str, content: &str) -> Message {
    Message::tool_response(id.to_string(), content.to_string())
}

fn finalized_result(id: &str, content: &str) -> ToolCallResult {
    ToolCallResult {
        tool_call_id: id.to_string(),
        llm_response: content.to_string(),
        audit_arguments: Vec::new(),
        audit_status: ToolAuditStatus::Success,
    }
}

fn tool_ids_after(messages: &[Message], assistant_index: usize) -> Vec<String> {
    messages
        .iter()
        .skip(assistant_index + 1)
        .take_while(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.clone())
        .collect()
}

fn content_for<'a>(messages: &'a [Message], call_id: &str) -> Option<&'a str> {
    messages
        .iter()
        .find(|message| message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id))
        .and_then(|message| match &message.content {
            MessageContent::Text(content) => Some(content.as_str()),
            MessageContent::Parts(_) => None,
        })
}

#[test]
fn repair_inserts_incomplete_result_before_later_messages() {
    let mut messages = vec![assistant([call("call-1")]), Message::user("continue".to_string())];

    let report = repair_unresolved_tool_calls(&mut messages).expect("unambiguous history should repair");

    assert_eq!(report.repaired_calls, 1);
    assert_eq!(tool_ids_after(&messages, 0), ["call-1"]);
    assert_eq!(messages[2].role, MessageRole::User);
    let content = content_for(&messages, "call-1").expect("recovery result should have text content");
    let recovery: serde_json::Value = serde_json::from_str(content).expect("recovery result should be JSON");
    assert_eq!(recovery["status"], "incomplete");
    assert_eq!(recovery["replayed"], false);
    let next_action = recovery["next_action"]
        .as_str()
        .expect("recovery result should have a next action")
        .to_ascii_lowercase();
    assert!(next_action.contains("verify"));
    assert!(next_action.contains("resubmit"));
}

#[test]
fn complete_history_is_preserved() {
    let mut messages = vec![assistant([call("call-1")]), tool_result("call-1", "real result")];
    let expected = messages.clone();

    let report = repair_unresolved_tool_calls(&mut messages).expect("complete history should be unambiguous");

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(messages, expected);
}

#[test]
fn recovery_rejects_duplicate_result_without_discarding_terminal_evidence() {
    // Review 3944816968: duplicate terminal results cannot be safely
    // attributed to one canonical provider result. Keep the evidence in
    // durable history and reject continuation instead of guessing.
    let mut messages = vec![
        assistant([call("call-1"), call("call-2")]),
        tool_result("call-1", "first result"),
        tool_result("call-1", "duplicate result"),
    ];

    let error = repair_unresolved_tool_calls(&mut messages).expect_err("duplicate results are ambiguous");

    assert_eq!(error, AmbiguousToolResultHistory);
    assert_eq!(tool_ids_after(&messages, 0), ["call-1", "call-1", "call-2"]);
    assert_eq!(content_for(&messages, "call-1"), Some("first result"));
    assert!(
        messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("call-1")
                && message.content.as_text() == "duplicate result"
        }),
        "duplicate terminal evidence must remain represented rather than being guessed away"
    );
    assert!(content_for(&messages, "call-2").is_some_and(|content| content.contains("\"status\":\"incomplete\"")));
}

#[test]
fn recovery_preserves_and_relocates_unique_non_contiguous_result() {
    // Review 3944816968: results arriving after an unrelated message are
    // not part of the preceding assistant batch and must not remain as a
    // second, non-contiguous result after recovery.
    let late_result = tool_result("call-1", "late result");
    let mut messages = vec![
        assistant([call("call-1"), call("call-2")]),
        Message::user("continue".to_string()),
        late_result.clone(),
    ];

    let report = repair_unresolved_tool_calls(&mut messages).expect("unique late result should be attributable");

    assert_eq!(report.repaired_calls, 1);
    assert_eq!(tool_ids_after(&messages, 0), ["call-1", "call-2"]);
    assert_eq!(messages[1], late_result);
    assert_eq!(messages[3].role, MessageRole::User);
    assert_eq!(messages.iter().filter(|message| message.role == MessageRole::Tool).count(), 2);
}

#[test]
fn stage_writes_one_incomplete_result_per_call() {
    let mut messages = vec![Message::user("edit files".to_string())];

    let call_ids = stage_tool_calls(&mut messages, assistant([call("call-1"), call("call-2")]));

    assert_eq!(call_ids, ["call-1", "call-2"]);
    assert_eq!(tool_ids_after(&messages, 1), ["call-1", "call-2"]);
}

#[test]
fn recovery_accepts_repeated_call_ids_in_separate_completed_batches() {
    let mut messages = vec![
        assistant([call("reused-call")]),
        tool_result("reused-call", "first result"),
        Message::user("continue".to_string()),
        assistant([call("reused-call")]),
        tool_result("reused-call", "second result"),
    ];

    let report =
        repair_unresolved_tool_calls(&mut messages).expect("separate assistant batches make the reused id unambiguous");

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(tool_ids_after(&messages, 0), ["reused-call"]);
    assert_eq!(tool_ids_after(&messages, 3), ["reused-call"]);
}

#[test]
fn successful_results_replace_incomplete_results() {
    let mut messages = Vec::new();
    drop(stage_tool_calls(&mut messages, assistant([call("call-1")])));
    let results = [finalized_result("call-1", "real result")];

    let replaced = replace_tool_results(&mut messages, &results);

    assert_eq!(replaced, 1);
    assert_eq!(content_for(&messages, "call-1"), Some("real result"));
}

#[test]
fn replace_tool_results_updates_the_latest_staged_result_for_a_reused_call_id() {
    // Reused ids across separate completed batches are accepted, so a later
    // batch's placeholder is the newest match. Replacing the first match would
    // overwrite an earlier terminal result and leave the new one incomplete.
    let mut messages = Vec::new();
    drop(stage_tool_calls(&mut messages, assistant([call("call-reused")])));
    let replaced_first = replace_tool_results(&mut messages, &[finalized_result("call-reused", "first result")]);
    drop(stage_tool_calls(&mut messages, assistant([call("call-reused")])));
    let replaced_second = replace_tool_results(&mut messages, &[finalized_result("call-reused", "second result")]);

    assert_eq!(replaced_first, 1);
    assert_eq!(replaced_second, 1);
    assert_eq!(
        messages.iter().filter(|message| message.role == MessageRole::Assistant).count(),
        2,
        "each staged batch keeps its assistant request"
    );
    let tool_messages = messages
        .iter()
        .filter(|message| message.role == MessageRole::Tool)
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 2, "each batch keeps exactly one result for the reused id");
    assert_eq!(tool_messages[0].content.as_text(), "first result");
    assert_eq!(tool_messages[1].content.as_text(), "second result");
    assert_eq!(messages[1].content.as_text(), "first result", "the earlier terminal result stays unchanged");
    assert_eq!(messages[3].content.as_text(), "second result", "the newest placeholder is the one replaced");
}

#[test]
fn recovery_accepts_a_truncated_leading_tool_result_run() {
    // Front compaction can retain tool results whose assistant request was
    // trimmed away. That leading run is the only shape truncation can produce
    // and must resume rather than being rejected as ambiguous.
    let mut messages = vec![
        tool_result("truncated-call", "earlier result"),
        assistant([call("live-call")]),
        tool_result("live-call", "live result"),
    ];
    let expected = messages.clone();

    let report =
        repair_unresolved_tool_calls(&mut messages).expect("a front-truncated tool-result prefix is not ambiguous");

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(report.relocated_calls, 0);
    assert_eq!(report.truncated_prefix, 1);
    assert_eq!(messages, expected);
}

#[test]
fn recovery_rejects_an_orphan_result_after_a_later_user_message() {
    // Only the leading run is exempt. An orphan result inside the retained
    // history remains ambiguous and must not be silently dropped.
    let mut messages = vec![
        tool_result("truncated-call", "earlier result"),
        assistant([call("live-call")]),
        tool_result("live-call", "live result"),
        Message::user("continue".to_string()),
        tool_result("orphan-call", "orphan result"),
    ];

    let error = repair_unresolved_tool_calls(&mut messages)
        .expect_err("an orphan result after a later user message is ambiguous");

    assert_eq!(error, AmbiguousToolResultHistory);
    assert!(
        messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message.tool_call_id.as_deref() == Some("orphan-call")
                && message.content.as_text() == "orphan result"
        }),
        "the orphan result must remain in durable history"
    );
}

#[test]
fn recovery_reports_relocation_without_inserting_placeholders() {
    let mut messages = vec![
        assistant([call("late-call")]),
        Message::user("continue".to_string()),
        tool_result("late-call", "late result"),
    ];

    let report = repair_unresolved_tool_calls(&mut messages)
        .expect("a unique late result without a missing call should relocate cleanly");

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(report.relocated_calls, 1);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert_eq!(messages[1].role, MessageRole::Tool);
    assert_eq!(messages[1].tool_call_id.as_deref(), Some("late-call"));
    assert_eq!(messages[1].content.as_text(), "late result");
    assert_eq!(messages[2].role, MessageRole::User);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1_024))]

    #[test]
    fn repair_is_complete_idempotent_and_preserves_real_results(
        cases in btree_map("[a-z][a-z0-9]{0,7}", (any::<bool>(), "[ -~]{0,32}"), 1..9),
    ) {
        let calls = cases.keys().map(|id| call(id)).collect::<Vec<_>>();
        let mut messages = vec![Message::system("before".to_string()), assistant(calls)];
        let expected_real = cases
            .iter()
            .filter(|(_, (completed, _))| *completed)
            .map(|(id, (_, content))| (id.clone(), content.clone()))
            .collect::<Vec<_>>();
        for (id, content) in &expected_real {
            messages.push(tool_result(id, content));
        }
        messages.push(Message::user("after".to_string()));
        let expected_missing = cases.len() - expected_real.len();

        let first = repair_unresolved_tool_calls(&mut messages)
            .expect("generated unique calls are unambiguous");
        let once = messages.clone();
        let second = repair_unresolved_tool_calls(&mut messages)
            .expect("repaired generated history remains unambiguous");

        prop_assert_eq!(first.repaired_calls, expected_missing);
        prop_assert_eq!(second.repaired_calls, 0);
        prop_assert_eq!(&messages, &once);

        let actual_ids = tool_ids_after(&messages, 1).into_iter().collect::<BTreeSet<_>>();
        let expected_ids = cases.keys().cloned().collect::<BTreeSet<_>>();
        prop_assert_eq!(actual_ids, expected_ids);
        for (id, expected_content) in expected_real {
            prop_assert_eq!(content_for(&messages, &id), Some(expected_content.as_str()));
        }
    }
}
