//! Exercises the ACP extension metrics the Lody management surface records:
//! the explicit-collector observation boundary and the request accounting that
//! belongs to the dispatch boundary alone.

use super::super::*;
use super::agent_with_launch_subagent_controller;
use assert_fs::TempDir;

/// The observation boundary takes its collector explicitly, so it can be
/// exercised without an agent, and it counts every operation of the surface.
#[tokio::test]
async fn observation_boundary_counts_every_operation_with_an_explicit_collector() {
    let workspace = TempDir::new().expect("observation boundary workspace");
    let agent = crate::zed::tests::build_agent(workspace.path()).await;
    let metrics = agent.local_tool_registry.metrics_collector();
    for method in [
        crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
        crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
    ] {
        let before = metrics.get_acp_extension_metrics();
        let observed = observe_management_request(metrics.as_ref(), method, "bounded-correlation", async {
            Ok(serde_json::json!({ "tasks": [] }))
        })
        .await
        .expect("the observation boundary passes a successful result through");
        assert_eq!(observed, serde_json::json!({ "tasks": [] }), "the boundary must not rewrite the result");
        let after = metrics.get_acp_extension_metrics();
        assert_eq!(
            after.management_requests_started,
            before.management_requests_started + 1,
            "{method} must be counted as a management request: {after:?}"
        );
        assert_eq!(
            after.management_requests_succeeded,
            before.management_requests_succeeded + 1,
            "{method} must record exactly one success: {after:?}"
        );
        assert_eq!(after.management_requests_in_flight, 0, "{method} must release its in-flight slot: {after:?}");
    }
}

/// Request accounting belongs to the dispatch boundary alone.
///
/// The query helpers are pure: calling `list_subagents` or `subagent_output`
/// directly records no management request. Each dispatch, whether it succeeds
/// or fails, records exactly one started request and one outcome. This test
/// observes the counters around a single call at a time, because the duplex
/// round trip only checks aggregate totals.
#[tokio::test]
async fn management_metrics_belong_to_the_dispatch_boundary() {
    let workspace = TempDir::new().expect("management metrics workspace");
    let agent = agent_with_launch_subagent_controller(workspace.path()).await;
    let session_id = agent.register_session().expect("register session").0.to_string();
    let baseline = acp_extension_metrics(&agent);

    // Direct query-helper calls are unobserved: a helper has no collector and
    // cannot record, so an embedder that queries task state pays no telemetry.
    let listed_by_helper =
        list_subagents(&agent, ListSubagentsRequest { session_id: session_id.clone(), active_only: false })
            .await
            .expect("list through the query helper");
    assert_eq!(
        listed_by_helper["tasks"].as_array().map(Vec::len),
        Some(0),
        "the query helper must answer with the session's tasks: {listed_by_helper}"
    );
    let unread = subagent_output(
        &agent,
        SubagentOutputRequest {
            session_id: session_id.clone(),
            task_id: "missing".to_string(),
            tail: None,
        },
    )
    .await
    .expect_err("the query helper keeps its unknown-task error");
    assert_eq!(
        unread.data.as_ref().and_then(|data| data.get("reason")).and_then(Value::as_str),
        Some("unknown_task"),
        "the query helper must keep the bounded reason"
    );
    let after_helpers = acp_extension_metrics(&agent);
    assert_eq!(
        (after_helpers.management_requests_started, after_helpers.management_requests_succeeded),
        (baseline.management_requests_started, baseline.management_requests_succeeded),
        "a direct query-helper call must record no management request: {after_helpers:?}"
    );
    assert_eq!(
        after_helpers.management_requests_failed, baseline.management_requests_failed,
        "a direct query-helper call must record no failure: {after_helpers:?}"
    );

    // A successful `list` dispatch records one started request and one success.
    let listed = handle_lody_subagent_management(
        &agent,
        crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
        Value::Object(
            [("sessionId".to_string(), Value::String(session_id.clone()))]
                .into_iter()
                .collect(),
        ),
    )
    .await
    .expect("dispatch list");
    assert_eq!(
        listed["tasks"].as_array().map(Vec::len),
        Some(0),
        "the dispatched list must answer with the session's tasks: {listed}"
    );
    let after_list = acp_extension_metrics(&agent);
    assert_eq!(
        after_list.management_requests_started,
        after_helpers.management_requests_started + 1,
        "one list dispatch records exactly one started request: {after_list:?}"
    );
    assert_eq!(
        after_list.management_requests_succeeded,
        after_helpers.management_requests_succeeded + 1,
        "a successful list dispatch records one success: {after_list:?}"
    );

    // An `output` query for an unknown task is a failing dispatch: it still
    // counts as a request, and the failure is recorded at the boundary.
    let output_error = handle_lody_subagent_management(
        &agent,
        crate::zed::agent::lody::LODY_SUBAGENTS_OUTPUT_METHOD,
        serde_json::json!({ "sessionId": session_id, "taskId": "missing" }),
    )
    .await
    .expect_err("an unknown task cannot be read");
    assert_eq!(
        output_error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str),
        Some("unknown_task"),
        "the output dispatch must keep its bounded reason"
    );
    let after_output = acp_extension_metrics(&agent);
    assert_eq!(
        after_output.management_requests_started,
        after_list.management_requests_started + 1,
        "one output dispatch records exactly one started request: {after_output:?}"
    );
    assert_eq!(
        after_output.management_requests_failed,
        after_list.management_requests_failed + 1,
        "a failed output dispatch records one failure: {after_output:?}"
    );

    // `cancel` is the only command RPC; its dispatch outcome is recorded the
    // same way, so a missing task is counted and reported as a failure.
    let cancel_error = handle_lody_subagent_management(
        &agent,
        crate::zed::agent::lody::LODY_SUBAGENTS_CANCEL_METHOD,
        serde_json::json!({ "sessionId": session_id, "taskId": "missing" }),
    )
    .await
    .expect_err("an unknown task cannot be cancelled");
    assert_eq!(
        cancel_error
            .data
            .as_ref()
            .and_then(|data| data.get("reason"))
            .and_then(Value::as_str),
        Some("unknown_task"),
        "the cancel dispatch must keep its bounded reason"
    );
    let after_cancel = acp_extension_metrics(&agent);
    assert_eq!(
        after_cancel.management_requests_started,
        after_output.management_requests_started + 1,
        "one cancel dispatch records exactly one started request: {after_cancel:?}"
    );
    assert_eq!(
        after_cancel.management_requests_failed,
        after_output.management_requests_failed + 1,
        "a failed cancel dispatch records one failure: {after_cancel:?}"
    );
    assert_eq!(
        after_cancel.management_requests_in_flight, 0,
        "every dispatch must release its in-flight slot: {after_cancel:?}"
    );
}

/// Reads the ACP extension counters the adapter records into.
pub(super) fn acp_extension_metrics(agent: &ZedAgent) -> diapason_core::metrics::AcpExtensionMetrics {
    agent.local_tool_registry.metrics_collector().get_acp_extension_metrics()
}
