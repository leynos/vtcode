//! Validates the parent `tool_recovery` module's history repair.
//!
//! The cases cover write-ahead staging of incomplete placeholders, their
//! replacement by terminal results, repair of legacy archives that lack
//! results, rejection of ambiguous attribution, and idempotence when recovery
//! runs over already-repaired history.

use super::*;
use diapason_core::llm::provider::MessageContent;
use diapason_safety::audit_log::ToolAuditStatus;
use proptest::collection::btree_map;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

/// Builds an `apply_patch` tool call with the given ID.
fn call(id: &str) -> ToolCall {
    ToolCall::function(id.to_string(), "apply_patch".to_string(), "{}".to_string())
}

/// Builds an assistant message carrying the given tool calls.
fn assistant(calls: impl IntoIterator<Item = ToolCall>) -> Message {
    Message::assistant_with_tools(String::new(), calls.into_iter().collect())
}

/// Builds a tool response message for the given call ID.
fn tool_result(id: &str, content: &str) -> Message {
    Message::tool_response(id.to_string(), content.to_string())
}

/// Builds a successful terminal result for the given call ID.
fn finalized_result(id: &str, content: &str) -> ToolCallResult {
    ToolCallResult {
        tool_call_id: id.to_string(),
        llm_response: content.to_string(),
        audit_arguments: Vec::new(),
        audit_status: ToolAuditStatus::Success,
    }
}

/// Returns the contiguous tool-result IDs following an assistant message.
fn tool_ids_after(messages: &[Message], assistant_index: usize) -> Vec<String> {
    messages
        .iter()
        .skip(assistant_index + 1)
        .take_while(|message| message.role == MessageRole::Tool)
        .filter_map(|message| message.tool_call_id.clone())
        .collect()
}

/// Returns the text content of the tool result for a call ID.
fn content_for<'a>(messages: &'a [Message], call_id: &str) -> Option<&'a str> {
    messages
        .iter()
        .find(|message| message.role == MessageRole::Tool && message.tool_call_id.as_deref() == Some(call_id))
        .and_then(|message| match &message.content {
            MessageContent::Text(content) => Some(content.as_str()),
            MessageContent::Parts(_) => None,
        })
}

/// Inserts one incomplete result before a later user message.
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

/// Leaves a complete history unchanged.
#[test]
fn complete_history_is_preserved() {
    let mut messages = vec![assistant([call("call-1")]), tool_result("call-1", "real result")];
    let expected = messages.clone();

    let report = repair_unresolved_tool_calls(&mut messages).expect("complete history should be unambiguous");

    assert_eq!(report.repaired_calls, 0);
    assert_eq!(messages, expected);
}

/// Rejects duplicate results while keeping both as durable evidence.
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

/// Relocates a unique late result beside its request batch.
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

/// Stages exactly one incomplete result per tool call.
#[test]
fn stage_writes_one_incomplete_result_per_call() {
    let mut messages = vec![Message::user("edit files".to_string())];

    let call_ids = stage_tool_calls(&mut messages, assistant([call("call-1"), call("call-2")]));

    assert_eq!(call_ids, ["call-1", "call-2"]);
    assert_eq!(tool_ids_after(&messages, 1), ["call-1", "call-2"]);
}

/// Accepts reused call IDs across separate completed batches.
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

/// Replaces a staged placeholder with its terminal result.
#[test]
fn successful_results_replace_incomplete_results() {
    let mut messages = Vec::new();
    drop(stage_tool_calls(&mut messages, assistant([call("call-1")])));
    let results = [finalized_result("call-1", "real result")];

    let replaced = replace_tool_results(&mut messages, &results);

    assert_eq!(replaced, 1);
    assert_eq!(content_for(&messages, "call-1"), Some("real result"));
}

/// Replaces the newest placeholder when a call ID is reused.
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

/// Accepts a leading run of tool results left by front compaction.
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

/// Rejects an orphan result that follows a later user message.
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

/// Reports a relocation without inserting any placeholders.
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

    /// Repairs generated histories completely and idempotently while
    /// preserving real results.
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

/// Result disposition for one generated tool call in a multi-batch history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Disposition {
    /// No terminal result is present; recovery must stage a placeholder.
    Missing,
    /// A terminal result follows its request batch contiguously.
    Contiguous,
    /// A terminal result arrives after an intervening non-tool message.
    Late,
}

/// A generated tool call: a small-alphabet ID and its result disposition.
type GeneratedCall = (u8, Disposition);

/// Generates a small-alphabet call ID so later batches can reuse it.
fn call_id_strategy() -> impl Strategy<Value = u8> {
    0u8..3
}

/// Generates one result disposition for a generated call.
fn disposition_strategy() -> impl Strategy<Value = Disposition> {
    prop_oneof![
        Just(Disposition::Missing),
        Just(Disposition::Contiguous),
        Just(Disposition::Late),
    ]
}

/// Generates one batch of calls with per-call result dispositions.
fn batch_strategy() -> impl Strategy<Value = Vec<GeneratedCall>> {
    prop::collection::vec((call_id_strategy(), disposition_strategy()), 1..=3)
}

/// Generates several batches so call IDs can be reused across them.
fn batches_strategy() -> impl Strategy<Value = Vec<Vec<GeneratedCall>>> {
    prop::collection::vec(batch_strategy(), 1..=4)
}

/// Formats a generated call ID as a stable tool-call identifier.
fn call_id_text(id: u8) -> String {
    format!("call-{id}")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(384))]

    /// Generated multi-batch histories with reused IDs, late results, and
    /// missing results repair completely and idempotently while preserving
    /// every terminal result.
    #[test]
    fn multi_batch_repair_preserves_results_and_is_idempotent(generated in batches_strategy()) {
        // A batch with duplicate IDs is ambiguous by construction, so keep
        // only the first occurrence of each ID within a batch. Reused IDs
        // across batches remain and exercise the reused-ID path.
        let mut batches = generated
            .into_iter()
            .map(|batch| {
                let mut seen = BTreeSet::new();
                batch
                    .into_iter()
                    .filter(|(id, _)| seen.insert(*id))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();

        // A late result is only attributable when its ID is unique across
        // every batch. Downgrade late results for reused IDs to contiguous
        // results so the generated history stays accepted; non-unique late
        // results are rejected by the ambiguity property instead.
        let mut id_counts = BTreeMap::new();
        for batch in &batches {
            for (id, _) in batch {
                *id_counts.entry(*id).or_insert(0usize) += 1;
            }
        }
        for batch in &mut batches {
            for (id, disposition) in batch.iter_mut() {
                if *disposition == Disposition::Late && id_counts[&*id] > 1 {
                    *disposition = Disposition::Contiguous;
                }
            }
        }

        let mut messages = Vec::new();
        let mut expected_terminal = Vec::new();
        let mut missing_count = 0usize;
        let mut late_count = 0usize;
        let mut total_calls = 0usize;

        for (batch_index, batch) in batches.iter().enumerate() {
            let calls = batch
                .iter()
                .map(|(id, _)| call(&call_id_text(*id)))
                .collect::<Vec<_>>();
            messages.push(assistant(calls));
            total_calls += batch.len();

            for (call_index, (id, disposition)) in batch.iter().enumerate() {
                match *disposition {
                    Disposition::Contiguous => {
                        let content = format!("terminal-{batch_index}-{call_index}");
                        messages.push(tool_result(&call_id_text(*id), &content));
                        expected_terminal.push(content);
                    }
                    Disposition::Missing => missing_count += 1,
                    Disposition::Late => late_count += 1,
                }
            }

            messages.push(Message::user("continue".to_string()));

            for (call_index, (id, disposition)) in batch.iter().enumerate() {
                if *disposition == Disposition::Late {
                    let content = format!("terminal-{batch_index}-{call_index}");
                    messages.push(tool_result(&call_id_text(*id), &content));
                    expected_terminal.push(content);
                }
            }
        }

        let first = repair_unresolved_tool_calls(&mut messages)
            .expect("generated multi-batch history should be unambiguous");
        let once = messages.clone();
        let second = repair_unresolved_tool_calls(&mut messages)
            .expect("repaired multi-batch history should remain unambiguous");

        prop_assert_eq!(first.repaired_calls, missing_count);
        prop_assert_eq!(first.relocated_calls, late_count);
        prop_assert_eq!(second.repaired_calls, 0);
        prop_assert_eq!(second.relocated_calls, 0);
        prop_assert_eq!(&messages, &once);

        let tool_messages = messages
            .iter()
            .filter(|message| message.role == MessageRole::Tool)
            .collect::<Vec<_>>();
        prop_assert_eq!(tool_messages.len(), total_calls);

        let placeholder_count = tool_messages
            .iter()
            .filter(|message| message.content.as_text().contains("\"status\":\"incomplete\""))
            .count();
        prop_assert_eq!(placeholder_count, missing_count);

        for content in &expected_terminal {
            prop_assert!(
                tool_messages.iter().any(|message| message.content.as_text() == content.as_str()),
                "terminal result {content} must be preserved"
            );
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A duplicated terminal result for one call is rejected as ambiguous
    /// without changing the history.
    #[test]
    fn duplicate_terminal_result_is_rejected_without_changing_history(
        call_ids in prop::collection::vec(call_id_strategy(), 1..=3),
        duplicate_index in any::<prop::sample::Index>(),
    ) {
        let mut unique_ids = Vec::new();
        for id in call_ids {
            if !unique_ids.contains(&id) {
                unique_ids.push(id);
            }
        }

        let calls = unique_ids
            .iter()
            .map(|id| call(&call_id_text(*id)))
            .collect::<Vec<_>>();
        let mut messages = vec![assistant(calls)];
        for id in &unique_ids {
            messages.push(tool_result(&call_id_text(*id), &format!("terminal-{id}")));
        }
        messages.push(Message::user("continue".to_string()));
        let duplicate = unique_ids[duplicate_index.index(unique_ids.len())];
        messages.push(tool_result(&call_id_text(duplicate), "duplicate result"));
        let before = messages.clone();

        let error = repair_unresolved_tool_calls(&mut messages)
            .expect_err("a duplicated terminal result cannot be attributed uniquely");

        prop_assert_eq!(error, AmbiguousToolResultHistory);
        prop_assert_eq!(&messages, &before);
    }
}

/// Fixes the durable placeholder schema that archives and agents depend on.
///
/// The placeholder is a stable structured output: recovery consumers read its
/// status, state, and next action, so a field rename or reordering is a
/// contract change rather than an incidental refactor.
#[test]
fn incomplete_placeholder_schema_is_stable() {
    let message = incomplete_tool_result(&call("call-placeholder"));

    assert_eq!(message.role, MessageRole::Tool);
    assert_eq!(message.tool_call_id.as_deref(), Some("call-placeholder"));
    let placeholder: serde_json::Value =
        serde_json::from_str(&message.content.as_text()).expect("placeholder content must be JSON");
    insta::assert_json_snapshot!(placeholder, @r###"
    {
      "status": "incomplete",
      "tool": "apply_patch",
      "message": "Tool execution did not reach a durable terminal result. Its side effects are uncertain.",
      "replayed": false,
      "state": "uncertain",
      "next_action": "Verify the current workspace state, then resubmit the tool call only if the requested work is still needed."
    }
    "###);
}
