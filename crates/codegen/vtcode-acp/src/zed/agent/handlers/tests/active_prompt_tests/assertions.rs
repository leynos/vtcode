//! Wire and notification assertion helpers for the ACP active-prompt regression.

use super::BRANCH;
use crate::acp;
use serde_json::Value;
use vtcode_core::llm::provider::Message;

/// Assert every fixture read tool call completed successfully.
pub(super) fn assert_completed_reads(notifications: &[acp::AgentNotification]) {
    let successful_reads = notifications
        .iter()
        .filter_map(|notification| {
            let acp::AgentNotification::SessionNotification(notification) = notification else {
                return None;
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return None;
            };
            (update.tool_call_id.0.starts_with("issue164-list-")
                && update.fields.status == Some(acp::ToolCallStatus::Completed))
            .then(|| update.tool_call_id.0.to_string())
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(successful_reads.len(), 12, "all fixture reads must actually succeed");
}

/// Assert at least two production automatic compactions completed in the turn.
pub(super) fn assert_completed_compactions(notifications: &[acp::AgentNotification]) {
    let completed_compactions = notifications
        .iter()
        .filter(|notification| {
            let acp::AgentNotification::SessionNotification(notification) = notification else {
                return false;
            };
            let acp::SessionUpdate::ToolCallUpdate(update) = &notification.update else {
                return false;
            };
            update.tool_call_id.0.starts_with("activity:compaction:")
                && update.fields.status == Some(acp::ToolCallStatus::Completed)
        })
        .count();
    assert!(
        completed_compactions >= 2,
        "must complete two production automatic compactions in the original ACP turn"
    );
}

/// Assert the summary input contains the completed work and the task identity.
pub(super) fn assert_summary_describes_active_work(summary: &Value) {
    let text = summary["messages"][0]["content"].as_str().expect("summary input");
    assert!(text.contains("ISSUE164_WORK"), "summary input must include actual completed work");
    assert!(
        text.contains(BRANCH) || text.contains("ISSUE164_SUMMARY"),
        "summary input must describe the actual task"
    );
}

/// Assert the next provider request carries the generated summary as a system
/// message.
pub(super) fn assert_generated_summary_installed(request: &Value) {
    assert!(
        request["messages"]
            .as_array()
            .expect("compacted HTTP messages")
            .iter()
            .any(|message| {
                message["role"] == "system"
                    && message["content"]
                        .as_str()
                        .is_some_and(|text| text.contains("ISSUE164_SUMMARY"))
            }),
        "successful automatic compaction must install the generated summary before the next provider call"
    );
}

/// Assert the active messages exceed the old 20k continuity allowance.
pub(super) fn assert_old_continuity_budget_exceeded(request: &Value) {
    // A BPE estimate establishes actual pressure on the old 20k allowance.
    let active_tokens: usize = request["messages"]
        .as_array()
        .expect("active messages")
        .iter()
        .filter_map(|message| message["content"].as_str())
        .map(|text| Message::assistant(text.to_string()).estimate_tokens())
        .sum();
    assert!(active_tokens > 20_000, "exercise the old continuity clipping budget: {active_tokens}");
}
