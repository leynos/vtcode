//! Generated-history properties for the parent `tool_recovery` module.

use super::*;
use proptest::collection::btree_map;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet};

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
proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// A late result for a call ID reused across batches cannot be attributed
    /// to one batch, so recovery rejects the history instead of guessing.
    #[test]
    fn reused_id_late_results_are_rejected_as_ambiguous(
        reused in call_id_strategy(),
        filler_len in 0usize..=3,
        late_position in any::<prop::sample::Index>(),
    ) {
        // Two batches request the same ID, so a later result for it is not
        // uniquely attributable to either batch.
        let other = reused.wrapping_add(1) % 3;
        let mut messages = vec![
            assistant(vec![call(&call_id_text(other)), call(&call_id_text(reused))]),
            tool_result(&call_id_text(other), "first-terminal"),
            Message::user("continue".to_string()),
            assistant(vec![call(&call_id_text(reused))]),
            Message::user("continue".to_string()),
        ];
        let mut trailing = (0..filler_len)
            .map(|_| Message::user("again".to_string()))
            .collect::<Vec<_>>();
        let at = late_position.index(trailing.len() + 1);
        trailing.insert(at, tool_result(&call_id_text(reused), "late-terminal"));
        messages.extend(trailing);

        let result = repair_unresolved_tool_calls(&mut messages);

        prop_assert!(
            matches!(result, Err(AmbiguousToolResultHistory)),
            "a reused ID with a late result must be rejected as ambiguous"
        );
        prop_assert!(
            messages.iter().any(|message| message.content.as_text() == "late-terminal"),
            "rejection must not silently drop the ambiguous terminal result"
        );
    }
}
