//! Runs the management examples published in the ACP guides.
//!
//! The request and response shapes in `docs/acp/ACP_INTEGRATION.md` and
//! `docs/acp/ACP_QUICK_REFERENCE.md` are the contract clients code against, so
//! these tests load the shipped text through the shared example loader and
//! decode it with the same types and projection the handlers use. A guide that
//! drifts from the wire shape then fails here instead of misleading a client
//! author.

#[path = "../../../../diapason-core/tests/support/doc_examples.rs"]
mod doc_examples;

use super::projection::managed_child_task;
use super::*;
use doc_examples::load_example;
use serde_json::{Value, json};

/// The guides that publish the Lody management examples, with their identifier
/// prefixes.
const GUIDES: [(&str, &str); 2] = [
    ("docs/acp/ACP_INTEGRATION.md", "acp-integration"),
    ("docs/acp/ACP_QUICK_REFERENCE.md", "acp-quick-reference"),
];

/// Loads one marked subagent example from a guide as JSON.
fn example(document: &str, prefix: &str, name: &str) -> Value {
    let identifier = format!("{prefix}-subagent-{name}");
    let example = load_example(document, &identifier)
        .unwrap_or_else(|error| panic!("{document} must publish {identifier}: {error}"));
    assert_eq!(example.language, "json", "{identifier} must stay a JSON example");
    serde_json::from_str(&example.body)
        .unwrap_or_else(|error| panic!("{identifier} must be a valid JSON body: {error}"))
}

/// Every published request decodes with the handler's own request types.
#[test]
fn published_requests_decode_with_the_handler_types() {
    for (document, prefix) in GUIDES {
        let list: ListSubagentsRequest = serde_json::from_value(example(document, prefix, "list-request"))
            .unwrap_or_else(|error| panic!("{document} list request must decode: {error}"));
        assert_eq!(list.session_id, "session-id");
        assert!(!list.active_only, "{document} must show the documented activeOnly default of false");

        let cancel: CancelSubagentRequest = serde_json::from_value(example(document, prefix, "cancel-request"))
            .unwrap_or_else(|error| panic!("{document} cancel request must decode: {error}"));
        assert_eq!(cancel.session_id, "session-id");
        assert_eq!(cancel.task_id, "task-id");
        assert_eq!(cancel.reason, None, "{document} must not present a cancel reason as part of the required shape");

        let output: SubagentOutputRequest = serde_json::from_value(example(document, prefix, "output-request"))
            .unwrap_or_else(|error| panic!("{document} output request must decode: {error}"));
        assert_eq!(output.session_id, "session-id");
        assert_eq!(output.task_id, "task-id");
        assert_eq!(
            normalize_output_tail_lines(output.tail).expect("the published tail must be within the transport bound"),
            200,
            "{document} must publish the documented default tail of 200 lines"
        );
    }
}

/// Every published response matches what the handler returns.
#[test]
fn published_responses_match_the_handler_shapes() {
    for (document, prefix) in GUIDES {
        assert_eq!(
            example(document, prefix, "cancel-response"),
            json!({}),
            "{document} must document the empty cancellation response"
        );

        let output = example(document, prefix, "output-response");
        assert!(output["output"].is_string(), "{document} must document a string output tail: {output}");
    }
}

/// The published task object is exactly what the management projection emits.
///
/// The fixture mirrors the guide's own identifiers and start instant, so a
/// renamed field, a changed status mapping, or a timestamp-format change breaks
/// this test rather than silently editing what clients are told to expect.
#[test]
fn published_list_response_matches_the_management_projection() {
    let entry: SubagentStatusEntry = serde_json::from_value(json!({
        "id": "task-id",
        "session_id": "agent-session-id",
        "parent_thread_id": "session-id",
        "agent_name": "reviewer",
        "display_label": "Reviewer",
        "description": "Review the parser",
        "source": "builtin",
        "status": "running",
        "background": false,
        "depth": 1,
        "created_at": "2025-08-28T00:00:00Z",
        "updated_at": "2025-08-28T00:00:00Z"
    }))
    .expect("the projection fixture must decode");

    let projected = json!({ "tasks": [managed_child_task(&entry)] });
    for (document, prefix) in GUIDES {
        assert_eq!(
            example(document, prefix, "list-response"),
            projected,
            "{document} must publish the task object the management handler returns"
        );
    }
}

/// The published usage example is what the adapter actually serializes.
///
/// The response below repeats the guide's own counts and model name, and the
/// comparison runs through `response_usage_notification`, so this test fails
/// when the serializer's field names, optional-count handling, or model key
/// diverge from the documented notification. Asserting the example's shape by
/// hand would let the two drift apart while both stayed internally consistent.
#[test]
fn published_usage_update_matches_the_serialised_notification() {
    let example = load_example("docs/acp/ACP_QUICK_REFERENCE.md", "acp-quick-reference-usage-update")
        .expect("the quick reference must publish a marked usage example");
    assert_eq!(example.language, "json", "the usage example must stay a JSON notification body");
    let published: Value = serde_json::from_str(&example.body).expect("the usage example must be valid JSON");

    let response = diapason_core::llm::provider::LLMResponse {
        usage: Some(diapason_core::llm::Usage {
            prompt_tokens: 123,
            completion_tokens: 57,
            total_tokens: 180,
            cache_read_tokens: Some(0),
            reasoning_output_tokens: Some(12),
            ..diapason_core::llm::Usage::default()
        }),
        ..diapason_core::llm::provider::LLMResponse::new("model-name", "answer")
    };
    let notification = crate::zed::agent::lody_usage::response_usage_notification(
        &acp::SessionId::new(Arc::from("session-id")),
        "fallback-model",
        &response,
        diapason_core::llm::usage_cost::UsageProfile {
            context_window: Some(524_288),
            ..diapason_core::llm::usage_cost::UsageProfile::default()
        },
    )
    .expect("serializing the published counts must succeed")
    .expect("a response carrying usage must publish a notification");

    assert_eq!(
        notification.method.as_ref(),
        crate::zed::agent::lody_usage::LODY_SESSION_USAGE_UPDATE_METHOD,
        "the published example must use the adapter's extension method"
    );
    let serialized: Value =
        serde_json::from_str(notification.params.get()).expect("the serialized notification must be JSON");
    assert_eq!(serialized, published, "the quick reference must publish exactly what the adapter serializes");
}
