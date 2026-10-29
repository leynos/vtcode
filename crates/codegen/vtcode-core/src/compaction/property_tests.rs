//! Generative contracts for source anchors, protocol groups, and context admission.
//!
//! Histories have 2–6 uniquely identified users, 1–3 batches per turn and 1–3
//! calls per batch. Key-sorted results cover permutations without rejection;
//! separately tagged legacy results intentionally lack originating call metadata.
//! Users carry up to 128 characters; tool bodies repeat such payloads 0–16 times.
//! Faults are appended at a known boundary, giving an oracle independent of the
//! production pending-batch state machine. Payloads and budgets vary; these are
//! bounded samples (proptest's default 256 cases per property), not a proof or
//! coverage of provider I/O, scheduling, or arbitrary persisted corruption.

use super::*;
use crate::llm::provider::{LLMError, LLMResponse};
use async_trait::async_trait;
use proptest::prelude::*;
use proptest::test_runner::TestCaseResult;
use std::collections::BTreeSet;
use vtcode_commons::llm::ToolCall;
use vtcode_commons::message_metadata::MessageMetadata;

struct GeneratedBatch {
    messages: Vec<Message>,
    call_ids: Vec<String>,
}

#[derive(Debug)]
struct BatchWitness {
    call_ids: Vec<String>,
}

#[derive(Debug)]
struct HistoryCase {
    messages: Vec<Message>,
    batches: Vec<BatchWitness>,
    legacy_ids: BTreeSet<String>,
}

#[derive(Debug)]
struct BatchSpec {
    calls: usize,
    result_keys: [u8; 3],
    payload: String,
}

/// Vary text, including multibyte content, without invalid Unicode or filtering.
fn payload() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 λ\n]{0,128}"
}

/// Sorting independent keys reaches every result permutation and shrinks safely.
fn batch_spec() -> impl Strategy<Value = BatchSpec> {
    (1usize..=3, any::<[u8; 3]>(), payload(), 0usize..=16).prop_map(|(calls, result_keys, text, repetitions)| {
        BatchSpec {
            calls,
            result_keys,
            payload: text.repeat(repetitions),
        }
    })
}

/// Build a valid history and record batch identities while constructing it.
fn histories() -> impl Strategy<Value = HistoryCase> {
    prop::collection::vec((payload(), prop::collection::vec(batch_spec(), 1..=3), any::<bool>()), 2..=6).prop_map(
        |turns| {
            let mut case = HistoryCase {
                messages: vec![Message::system("generated session summary".into())],
                batches: Vec::new(),
                legacy_ids: BTreeSet::new(),
            };
            for (turn_index, (text, batches, legacy)) in turns.into_iter().enumerate() {
                case.messages.push(user_anchor(turn_index, text));
                for (batch_index, spec) in batches.into_iter().enumerate() {
                    let prefix = format!("canonical-{turn_index}-{batch_index}");
                    let batch = batch_messages(&prefix, &spec);
                    case.messages.extend(batch.messages);
                    case.batches.push(BatchWitness { call_ids: batch.call_ids });
                }
                if legacy {
                    let id = format!("legacy-{turn_index}");
                    case.messages.push(Message::assistant(format!("legacy work {turn_index}")));
                    case.messages.push(Message::tool_response(id.clone(), "legacy result".into()));
                    case.legacy_ids.insert(id);
                }
            }
            case
        },
    )
}

/// Give otherwise identical user payloads distinct source identities/metadata.
fn user_anchor(index: usize, text: String) -> Message {
    let mut message = Message::user(format!("user-{index}: {text}"));
    message.metadata = Some(
        MessageMetadata::user_input(index as u64, message.estimate_tokens()).with_intent_id(format!("intent-{index}")),
    );
    message
}

/// Emit one canonical batch with source-order results witnessed independently.
fn batch_messages(prefix: &str, spec: &BatchSpec) -> GeneratedBatch {
    let call_ids: Vec<_> = (0..spec.calls).map(|index| format!("{prefix}-{index}")).collect();
    let calls = call_ids
        .iter()
        .map(|id| ToolCall::function(id.clone(), "read_file".into(), "{}".into()))
        .collect();
    let mut messages = vec![Message::assistant_with_tools(
        format!("work {prefix}: {}", spec.payload),
        calls,
    )];
    let mut result_order: Vec<_> = (0..spec.calls).collect();
    result_order.sort_by_key(|index| (spec.result_keys[*index], *index));
    messages.extend(
        result_order
            .into_iter()
            .map(|index| Message::tool_response(call_ids[index].clone(), format!("result {prefix}: {}", spec.payload))),
    );
    GeneratedBatch { messages, call_ids }
}

/// Locate the protected users solely by scanning the original source history.
fn check_source_anchors(source: &[Message], output: &[Message]) -> TestCaseResult {
    let users: Vec<_> = source.iter().filter(|message| message.role == MessageRole::User).collect();
    let first = users.first().expect("generator always supplies users");
    let last = users.last().expect("generator always supplies users");
    for anchor in [first, last] {
        prop_assert_eq!(output.iter().filter(|message| *message == *anchor).count(), 1);
    }
    let first_position = output.iter().position(|message| message == *first);
    let last_position = output.iter().position(|message| message == *last);
    prop_assert!(first_position < last_position);
    Ok(())
}

/// Check retained identities against source positions even when bodies are previews.
fn check_source_order(source: &[Message], output: &[Message]) -> TestCaseResult {
    let mut previous = None;
    for message in output {
        let position = match message.role {
            MessageRole::User => source.iter().position(|original| original == message),
            MessageRole::Tool => source.iter().position(|original| {
                original.role == MessageRole::Tool && original.tool_call_id == message.tool_call_id
            }),
            MessageRole::Assistant if message.tool_calls.is_some() => source.iter().position(|original| {
                original
                    .tool_calls
                    .as_ref()
                    .map(|calls| calls.iter().map(|call| &call.id).collect::<Vec<_>>())
                    == message
                        .tool_calls
                        .as_ref()
                        .map(|calls| calls.iter().map(|call| &call.id).collect::<Vec<_>>())
            }),
            _ => continue,
        };
        prop_assert!(position.is_some(), "retained message identity must originate in source");
        prop_assert!(previous.is_none_or(|prior| Some(prior) < position), "source order changed");
        previous = position;
    }
    Ok(())
}

/// Compare batches to construction witnesses; legacy results are exempt explicitly.
fn check_protocol(case: &HistoryCase, output: &[Message]) -> TestCaseResult {
    for (index, message) in output.iter().enumerate() {
        if let Some(calls) = message.tool_calls.as_ref().filter(|calls| !calls.is_empty()) {
            let ids: Vec<_> = calls.iter().map(|call| call.id.clone()).collect();
            prop_assert!(case.batches.iter().any(|batch| batch.call_ids == ids));
            check_batch_results(&ids, &output[index + 1..])?;
        }
        if message.role == MessageRole::Tool {
            check_result_origin(case, message, output)?;
        }
    }
    Ok(())
}

/// A retained canonical batch must have the exact witnessed result multiset.
fn check_batch_results(call_ids: &[String], following: &[Message]) -> TestCaseResult {
    let mut result_ids = following
        .iter()
        .take_while(|result| result.role == MessageRole::Tool)
        .map(|result| result.tool_call_id.clone())
        .collect::<Vec<_>>();
    let mut expected: Vec<_> = call_ids.iter().cloned().map(Some).collect();
    result_ids.sort();
    expected.sort();
    prop_assert_eq!(result_ids, expected, "a canonical batch must have exactly one result per call");
    Ok(())
}

/// Canonical results need one retained owner; tagged legacy results need none.
fn check_result_origin(case: &HistoryCase, result: &Message, output: &[Message]) -> TestCaseResult {
    let id = result.tool_call_id.as_ref().expect("generated results have IDs");
    if !case.legacy_ids.contains(id) {
        let introducing_batches = output
            .iter()
            .filter(|assistant| {
                assistant
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| calls.iter().any(|call| &call.id == id))
            })
            .count();
        prop_assert_eq!(introducing_batches, 1, "canonical results cannot survive without their batch");
    }
    Ok(())
}

/// Append one known-invalid batch; its origin is the independent prefix oracle.
fn append_fault(history: &mut Vec<Message>, kind: u8, keys: [u8; 3], text: String) -> usize {
    let origin = history.len();
    let spec = BatchSpec { calls: 3, result_keys: keys, payload: text };
    let mut batch = batch_messages("fault", &spec).messages;
    match kind {
        0 => {
            let calls = batch[0].tool_calls.as_mut().expect("constructed tool batch");
            calls[1].id = calls[0].id.clone();
        }
        1 => batch[0].tool_calls.as_mut().expect("constructed tool batch")[0].id.clear(),
        2 => {
            batch.pop();
        }
        3 => batch[2] = batch[1].clone(),
        4 => batch[1].tool_call_id = Some("unknown-result-id".into()),
        5 => batch.insert(1, Message::assistant("interrupted before results".into())),
        6 => batch[1].tool_call_id = None,
        _ => unreachable!("strategy limits fault kinds"),
    }
    history.extend(batch);
    origin
}

struct CapacityProvider(usize);

#[async_trait]
impl LLMProvider for CapacityProvider {
    fn name(&self) -> &str {
        "property-capacity"
    }
    fn supported_models(&self) -> Vec<String> {
        vec!["test".into()]
    }
    fn validate_request(&self, _request: &LLMRequest) -> Result<(), LLMError> {
        Ok(())
    }
    fn effective_context_size(&self, _model: &str) -> usize {
        self.0
    }
    async fn generate(&self, _request: LLMRequest) -> Result<LLMResponse, LLMError> {
        Ok(LLMResponse::new("test", "unused: these properties exercise pure admission"))
    }
}

proptest! {
    #[test]
    fn soft_selection_preserves_source_anchors_and_protocol(
        case in histories(), soft_target in 0usize..=4_096, keep_summary in any::<bool>(),
    ) {
        prop_assert_eq!(complete_protocol_prefix(&case.messages), case.messages.len());
        let selected = select_bounded_history(&case.messages, soft_target, keep_summary);
        check_source_anchors(&case.messages, &selected)?;
        check_source_order(&case.messages, &selected)?;
        check_protocol(&case, &selected)?;
        let source_tokens = case.messages.iter().map(Message::estimate_tokens).sum::<usize>();
        let roomy = select_bounded_history(&case.messages, source_tokens, true);
        prop_assert_eq!(&roomy, &case.messages, "a sufficient allowance retains every complete source unit");
        // A zero soft target still owes both complete anchors, without clipping.
        let zero_target = select_bounded_history(&case.messages, 0, false);
        check_source_anchors(&case.messages, &zero_target)?;
        prop_assert!(zero_target.iter().map(Message::estimate_tokens).sum::<usize>() > 0);
    }

    #[test]
    fn known_faults_cut_at_their_origin_and_do_not_leak(
        mut case in histories(), kind in 0u8..7, keys in any::<[u8; 3]>(), text in payload(),
    ) {
        let origin = append_fault(&mut case.messages, kind, keys, text);
        prop_assert_eq!(complete_protocol_prefix(&case.messages), origin);
        let normalized = normalize_provider_compacted_history(
            vec![Message::system("provider summary".into())], &case.messages,
        );
        check_source_anchors(&case.messages, &normalized)?;
        check_source_order(&case.messages, &normalized)?;
        check_protocol(&case, &normalized)?;
        prop_assert!(!normalized.iter().any(|message| message.tool_calls.as_ref()
            .is_some_and(|calls| calls.iter().any(|call| call.id.starts_with("fault-")))));
    }

    #[test]
    fn hard_finalizer_admits_fitting_anchors_within_model_budget(
        case in histories(), capacity in 2_048usize..=8_192,
    ) {
        let provider = CapacityProvider(capacity);
        // Public model-window policy reserves one eighth plus 512 tokens.
        let budget = capacity - capacity / 8 - 512;
        let users: Vec<_> = case.messages.iter().filter(|message| message.role == MessageRole::User).collect();
        prop_assert!(users.first().unwrap().estimate_tokens() + users.last().unwrap().estimate_tokens() <= budget);
        let bounded = bound_compacted_history_to_context(case.messages.clone(), &provider, "test")
            .map_err(|error| TestCaseError::fail(error.to_string()))?;
        prop_assert!(bounded.iter().map(Message::estimate_tokens).sum::<usize>() <= budget);
        if case.messages.iter().map(Message::estimate_tokens).sum::<usize>() <= budget {
            prop_assert_eq!(&bounded, &case.messages, "admissible history must be an exact no-op");
        }
        check_source_anchors(&case.messages, &bounded)?;
        check_source_order(&case.messages, &bounded)?;
        check_protocol(&case, &bounded)?;
    }

    #[test]
    fn oversized_anchors_refuse_without_changing_borrowed_history(
        mut case in histories(), capacity in 1_024usize..=4_096, oversized_first in any::<bool>(),
    ) {
        let user_index = if oversized_first {
            case.messages.iter().position(|message| message.role == MessageRole::User)
        } else {
            case.messages.iter().rposition(|message| message.role == MessageRole::User)
        }.expect("generator supplies source users");
        case.messages[user_index] = user_anchor(user_index, "must keep every byte ".repeat(capacity));
        let before = case.messages.clone();
        let provider = CapacityProvider(capacity);
        let summary = summary_history_for_request(&case.messages, &provider, "test");
        prop_assert!(summary.is_err());
        prop_assert!(summary.unwrap_err().to_string().contains("user instructions were left unchanged"));
        let bounded = bound_compacted_history_to_context(case.messages.clone(), &provider, "test");
        prop_assert!(bounded.is_err());
        prop_assert_eq!(&case.messages, &before);
    }
}
