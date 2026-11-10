//! Evidence provenance recorded by the parent `tool_recovery` module.
//!
//! A repair names each message it inserts or relocates together with the
//! history position it occupies, so a checkpoint can prove that exact slot
//! survived compaction even when a later batch reuses the call id.

use super::*;

/// A repaired batch names its own request before the results it made durable.
///
/// A later batch that reuses the call id can request and answer it alone, so
/// the request must be retained in place as well as the inserted placeholder.
#[test]
fn repaired_batch_reports_its_request_before_its_results() {
    let mut messages = vec![
        assistant([call("repaired-call")]),
        Message::user("continue".to_string()),
        assistant([call("later-call")]),
        tool_result("later-call", "later result"),
    ];

    let report = repair_unresolved_tool_calls(&mut messages).expect("unambiguous history should repair");

    assert_eq!(report.repaired_calls, 1);
    let evidence = &report.evidence;
    assert_eq!(evidence.len(), 2, "the batch reports its request and its placeholder");
    assert_eq!(evidence[0].message.role, MessageRole::Assistant);
    assert_eq!(
        evidence[0]
            .message
            .tool_calls
            .as_deref()
            .map(|calls| calls.iter().map(|call| call.id.as_str()).collect::<Vec<_>>()),
        Some(vec!["repaired-call"])
    );
    assert_eq!(evidence[1].message.role, MessageRole::Tool);
    assert_eq!(evidence[1].message.tool_call_id.as_deref(), Some("repaired-call"));
    assert_eq!(
        evidence.iter().map(|evidence| evidence.history_index).collect::<Vec<_>>(),
        vec![0, 1],
        "the evidence positions must match the repaired history"
    );
}

/// A relocation-only repair also names the request that owns the result.
#[test]
fn relocation_only_repair_reports_its_request_before_the_result() {
    let mut messages = vec![
        assistant([call("late-call")]),
        Message::user("continue".to_string()),
        tool_result("late-call", "late result"),
    ];

    let report = repair_unresolved_tool_calls(&mut messages).expect("a unique late result should relocate cleanly");

    assert_eq!(report.relocated_calls, 1);
    let evidence = &report.evidence;
    assert_eq!(evidence.len(), 2, "the batch reports its request and the relocated result");
    assert_eq!(evidence[0].message.role, MessageRole::Assistant);
    assert_eq!(evidence[1].message.role, MessageRole::Tool);
    assert_eq!(evidence[1].message.tool_call_id.as_deref(), Some("late-call"));
    assert_eq!(
        evidence.iter().map(|evidence| evidence.history_index).collect::<Vec<_>>(),
        vec![0, 1],
        "the relocated result keeps the position it was moved to"
    );
}

/// Records evidence positions even when an earlier batch reused the call id.
///
/// The two requests are byte-identical, so their content cannot identify the
/// repaired one. Recording the position keeps the retention check from naming
/// the earlier batch instead.
#[test]
fn repaired_evidence_records_the_position_of_a_reused_call_id() {
    let mut messages = vec![
        assistant([call("reused-call")]),
        tool_result("reused-call", "earlier result"),
        assistant([call("reused-call")]),
    ];

    let report = repair_unresolved_tool_calls(&mut messages).expect("unambiguous history should repair");

    assert_eq!(report.repaired_calls, 1);
    assert_eq!(
        report
            .evidence
            .iter()
            .map(|evidence| evidence.history_index)
            .collect::<Vec<_>>(),
        vec![2, 3],
        "the repaired request and its placeholder must keep their own positions"
    );
    assert_eq!(
        report.evidence[0].message.tool_calls.as_deref(),
        messages[2].tool_calls.as_deref(),
        "the earlier request is byte-identical, so only the position distinguishes them"
    );
}
