//! Property coverage for staged-batch retention across compaction boundaries.
use super::super::archive::{
    CheckpointDurability, missing_persisted_evidence, missing_persisted_tool_calls, missing_staged_batch_calls,
    required_checkpoint_evidence_violation, required_staged_batch_violation,
};
use diapason_core::llm::provider::{Message, MessageContent, MessageRole, ToolCall};
use diapason_core::utils::session_archive::SessionMessage;
use proptest::prelude::*;

/// Returns the id used for a generated call slot.
fn call_id(slot: u8) -> String {
    format!("call-{slot}")
}

/// Generates one to three batches of one or two call slots.
///
/// Slots are drawn from a small range so generated histories reuse call ids
/// across batches, which is the case the per-id retention check cannot see.
fn batch_slots() -> impl Strategy<Value = Vec<Vec<u8>>> {
    proptest::collection::vec(proptest::collection::vec(0u8..4, 1..3), 1..4)
}

/// Builds the persisted history for batches of call slots.
///
/// Each batch is one assistant request followed by one tool result per call,
/// matching the order the write-ahead checkpoint stages.
fn history_for(batches: &[Vec<u8>]) -> Vec<SessionMessage> {
    let mut messages = Vec::new();
    for batch in batches {
        let calls = batch
            .iter()
            .map(|slot| ToolCall::function(call_id(*slot), "read_file".to_string(), "{}".to_string()))
            .collect::<Vec<_>>();
        messages.push(SessionMessage::from(&Message::assistant_with_tools(String::new(), calls)));
        for slot in batch {
            messages.push(SessionMessage::from(&Message::tool_response(call_id(*slot), "result".to_string())));
        }
    }
    messages
}

/// Returns the call ids of the last batch of slots.
fn last_batch_ids(batches: &[Vec<u8>]) -> Vec<String> {
    batches
        .last()
        .expect("a generated history holds at least one batch")
        .iter()
        .map(|slot| call_id(*slot))
        .collect()
}

/// Returns the call ids requested by the last assistant tool batch.
fn last_requested_ids(messages: &[SessionMessage]) -> Vec<String> {
    messages
        .iter()
        .rev()
        .find(|message| {
            message.role == MessageRole::Assistant
                && message.tool_calls.as_deref().is_some_and(|calls| !calls.is_empty())
        })
        .and_then(|message| message.tool_calls.as_deref())
        .map(|calls| calls.iter().map(|call| call.id.clone()).collect())
        .unwrap_or_default()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// A history that still ends with the staged batch always verifies.
    #[test]
    fn a_retained_staged_batch_verifies(batches in batch_slots()) {
        let history = history_for(&batches);
        let staged = last_batch_ids(&batches);
        prop_assert!(missing_staged_batch_calls(&staged, &history).is_empty());
        prop_assert_eq!(
            required_staged_batch_violation(CheckpointDurability::Required, &staged, &history),
            None
        );
    }

    /// Compaction that drops the staged request is rejected, even when an
    /// older batch reused some of the same call ids.
    ///
    /// A byte-identical duplicate batch is the one indistinguishable case: the
    /// surviving history still requests and answers every staged id, so no
    /// content check can tell the batches apart. The archive repair path
    /// rejects that duplicated-result history as ambiguous before a checkpoint
    /// is attempted.
    #[test]
    fn a_dropped_staged_batch_is_rejected_despite_reused_ids(
        mut batches in batch_slots(),
        reused in proptest::collection::vec(0u8..4, 1..3),
    ) {
        batches.push(reused);
        let history = history_for(&batches);
        let staged = last_batch_ids(&batches);
        let staged_batch_index = history.len() - batches.last().expect("a generated batch").len() - 1;
        let mut compacted = history.clone();
        drop(compacted.remove(staged_batch_index));

        let surviving_requested = last_requested_ids(&compacted);
        if staged.iter().all(|call_id| surviving_requested.contains(call_id)) {
            // The surviving batch is an indistinguishable duplicate.
            return Ok(());
        }

        let missing = missing_staged_batch_calls(&staged, &compacted);
        prop_assert!(
            !missing.is_empty(),
            "a dropped staged batch must not survive verification: {compacted:?}"
        );
        prop_assert_eq!(
            required_staged_batch_violation(CheckpointDurability::Required, &staged, &compacted),
            Some(missing.len())
        );
    }
}

/// The compactor's ellipsis-truncated evidence still verifies in its slot.
///
/// `shrink_string` keeps a prefix and appends `"..."`, so the retained slot
/// holds a shortened form of the repaired result rather than the whole text.
#[test]
fn truncated_repaired_evidence_is_retained_in_its_slot() {
    let id = "truncated-evidence-call".to_string();
    let content = "a".repeat(64);
    let history = vec![
        request_for(&id),
        SessionMessage::with_tool_call_id(MessageRole::Tool, content.as_str(), Some(id)),
    ];
    let evidence = history[1].clone();
    let mut persisted = history.clone();
    persisted[1].content = MessageContent::Text(format!("{}...", "a".repeat(29)));

    assert!(
        missing_persisted_evidence(std::slice::from_ref(&evidence), &history, &persisted).is_empty(),
        "an ellipsis-truncated copy in the evidence slot must verify"
    );

    persisted[1].content = MessageContent::Text("unrelated truncated text...".to_string());
    assert_eq!(
        missing_persisted_evidence(std::slice::from_ref(&evidence), &history, &persisted),
        vec![evidence.tool_call_id.as_deref().expect("evidence call id").to_string()],
        "content that is not a truncation of the evidence must not verify"
    );
}

/// Builds one assistant request for `call_id`.
fn request_for(call_id: &str) -> SessionMessage {
    SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            call_id.to_string(),
            "read_file".to_string(),
            "{}".to_string(),
        )],
    ))
}

/// Builds the durable placeholder a repair inserts for an interrupted call.
fn repaired_placeholder(call_id: &str) -> SessionMessage {
    SessionMessage::with_tool_call_id(MessageRole::Tool, "{\"status\":\"incomplete\"}", Some(call_id.to_string()))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Compaction that drops repaired evidence is rejected even when a later
    /// completed batch reuses the repaired call id.
    ///
    /// The later batch requests and answers the id, so the ordered per-id check
    /// and the batch check both pass. Only the evidence's position shows that
    /// the repaired placeholder is gone.
    #[test]
    fn dropped_repaired_evidence_is_rejected_despite_reused_ids(slot in 0u8..4) {
        let id = call_id(slot);
        let request = request_for(&id);
        let placeholder = repaired_placeholder(&id);
        let later_result = SessionMessage::with_tool_call_id(
            MessageRole::Tool,
            "later terminal output",
            Some(id.clone()),
        );
        let history = vec![request.clone(), placeholder.clone(), request, later_result];
        let compacted = &history[2..];

        prop_assert!(
            missing_persisted_tool_calls(std::slice::from_ref(&id), compacted).is_empty(),
            "the later batch satisfies the ordered per-id check"
        );
        prop_assert!(
            missing_staged_batch_calls(std::slice::from_ref(&id), compacted).is_empty(),
            "the later batch satisfies the batch check"
        );
        prop_assert_eq!(
            missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, compacted),
            vec![id.clone()]
        );
        prop_assert_eq!(
            required_checkpoint_evidence_violation(
                CheckpointDurability::Required,
                std::slice::from_ref(&placeholder),
                &history,
                compacted,
            ),
            Some(1)
        );
        prop_assert!(
            missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, &history).is_empty(),
            "an uncompacted history retains the repaired evidence"
        );
    }

    /// A later result with the identical call id and identical content cannot
    /// stand in for dropped repaired evidence.
    ///
    /// Content equality and prefix tolerance both accept the later message; only
    /// its position shows that it is a different result.
    #[test]
    fn identical_later_result_does_not_replace_dropped_evidence(slot in 0u8..4) {
        let id = call_id(slot);
        let placeholder = repaired_placeholder(&id);
        let identical_later_result = placeholder.clone();
        let history = vec![
            request_for(&id),
            placeholder.clone(),
            SessionMessage::from(&Message::user("continue".to_string())),
            request_for(&id),
            identical_later_result,
        ];
        let compacted = &history[2..];

        prop_assert_eq!(
            missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, compacted),
            vec![id]
        );
    }

    /// A later result that starts with the repaired result's text cannot stand
    /// in for dropped repaired evidence.
    #[test]
    fn prefix_matching_later_result_does_not_replace_dropped_evidence(slot in 0u8..4) {
        let id = call_id(slot);
        let placeholder = repaired_placeholder(&id);
        let later_result = SessionMessage::with_tool_call_id(
            MessageRole::Tool,
            "{\"status\":\"incomplete\",\"detail\":\"later\"}",
            Some(id.clone()),
        );
        let history = vec![
            request_for(&id),
            placeholder.clone(),
            SessionMessage::from(&Message::user("continue".to_string())),
            request_for(&id),
            later_result,
        ];
        let compacted = &history[2..];

        prop_assert_eq!(
            missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, compacted),
            vec![id]
        );
    }

    /// Front trimming retains evidence exactly when it survives the boundary.
    #[test]
    fn evidence_retention_follows_the_compaction_boundary(
        dropped in 0usize..6,
        position in 0usize..6,
        slot in 0u8..4,
    ) {
        let id = call_id(slot);
        let placeholder = repaired_placeholder(&id);
        let mut history = (0..6)
            .map(|index| SessionMessage::from(&Message::user(format!("filler {index}"))))
            .collect::<Vec<_>>();
        history.insert(position, placeholder.clone());
        let dropped = dropped.min(history.len());
        let persisted = &history[dropped..];

        let missing = missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, persisted);
        if position < dropped {
            prop_assert_eq!(missing, vec![id]);
        } else {
            prop_assert!(missing.is_empty(), "evidence inside the retained suffix must verify");
        }
    }

    /// A result that precedes every request for its id does not retain the
    /// call, even though the id appears on both sides of the history.
    #[test]
    fn a_result_before_its_request_is_not_retained(slot in 0u8..4) {
        let id = call_id(slot);
        let result = SessionMessage::from(&Message::tool_response(id.clone(), "result".to_string()));
        let history = vec![result, request_for(&id)];

        prop_assert_eq!(
            missing_persisted_tool_calls(std::slice::from_ref(&id), &history),
            vec![id]
        );
    }
}

/// Front trimming that drops only the repaired batch's request is rejected.
///
/// The repaired result keeps its slot and a later batch reuses the call id, so
/// the ordered per-id check, the batch check, and the result's positional check
/// all pass. Only the request's own position shows that the batch it names was
/// lost.
#[test]
fn dropped_repaired_request_is_rejected_despite_reused_ids() {
    let id = "repaired-request-call".to_string();
    let request = request_for(&id);
    let placeholder = repaired_placeholder(&id);
    let history = vec![
        request.clone(),
        placeholder.clone(),
        SessionMessage::from(&Message::user("continue".to_string())),
        request_for(&id),
        SessionMessage::with_tool_call_id(MessageRole::Tool, "later terminal output", Some(id.clone())),
    ];
    assert!(
        missing_persisted_evidence(&[request.clone(), placeholder.clone()], &history, &history).is_empty(),
        "an uncompacted history retains the request and its result"
    );

    // Front compaction removes exactly the repaired batch's request.
    let compacted = &history[1..];

    assert!(
        missing_persisted_tool_calls(std::slice::from_ref(&id), compacted).is_empty(),
        "the later batch satisfies the ordered per-id check"
    );
    assert!(
        missing_staged_batch_calls(std::slice::from_ref(&id), compacted).is_empty(),
        "the later batch satisfies the batch check"
    );
    assert!(
        missing_persisted_evidence(std::slice::from_ref(&placeholder), &history, compacted).is_empty(),
        "the repaired result itself keeps its slot"
    );
    assert_eq!(
        missing_persisted_evidence(&[request, placeholder], &history, compacted),
        vec![format!("request:{id}")],
        "a dropped request must be reported even when the result survives"
    );
}

/// Another empty assistant request cannot stand in for a dropped one.
///
/// Repaired requests are normally empty-bodied and carry no `tool_call_id`, so
/// only the requested call ids distinguish them.
#[test]
fn a_different_request_does_not_replace_dropped_request_evidence() {
    let repaired_id = "repaired-request-call".to_string();
    let other_id = "other-request-call".to_string();
    let history = vec![
        request_for(&repaired_id),
        request_for(&other_id),
        SessionMessage::with_tool_call_id(MessageRole::Tool, "other output", Some(other_id)),
    ];
    let compacted = &history[1..];

    assert_eq!(
        missing_persisted_evidence(&[request_for(&repaired_id)], &history, compacted),
        vec![format!("request:{repaired_id}")]
    );
}

/// An empty relocated tool result verifies unchanged in its slot.
///
/// A tool that returned no output is legitimate evidence, so the comparison
/// must accept equality before considering truncation.
#[test]
fn an_empty_result_is_retained_in_its_slot() {
    let id = "empty-result-call".to_string();
    let request = request_for(&id);
    let empty_result = SessionMessage::with_tool_call_id(MessageRole::Tool, "", Some(id.clone()));
    let history = vec![request.clone(), empty_result.clone()];

    assert!(
        missing_persisted_evidence(&[request, empty_result], &history, &history).is_empty(),
        "unchanged empty evidence must verify without compaction"
    );
}

/// Older batches that between them requested and answered every staged id
/// blind the per-id check, while the batch check still sees that the surviving
/// request never made the whole staged batch.
#[test]
fn a_reused_batch_is_rejected_by_the_batch_check() {
    let history = history_for(&[vec![1], vec![2], vec![1, 2]]);
    let staged = vec![call_id(1), call_id(2)];
    let mut compacted = history.clone();
    // Drop the third assistant request, keeping both of its results.
    drop(compacted.remove(4));

    assert!(
        missing_persisted_tool_calls(&staged, &compacted).is_empty(),
        "the per-id check cannot see a drop whose ids older batches reused"
    );
    assert_eq!(
        missing_staged_batch_calls(&staged, &compacted),
        vec![call_id(1)],
        "the batch check must reject the call the surviving request never made"
    );
}
