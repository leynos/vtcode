//! Management-surface assertions for the Lody ACP extensions.
use super::*;
use diapason_core::subagents::TaskOwner;

/// Spawns through a detached session registry view.
///
/// The route must execute through the session's own controller and never fall
/// back to the shared runtime's ownerless controller once the view is detached.
/// Returns the id of the sibling task the session-owned route created.
pub(super) async fn exercise_detached_session_registry(
    runtime: &crate::zed::agent::SessionWorkspaceRuntime,
    controller: &Arc<diapason_core::subagents::SubagentController>,
    session_id: &acp::SessionId,
    shared_exec_session_id: &str,
) -> String {
    let registry = runtime
        .local_tool_registry
        .with_subagent_controller(Some(Arc::clone(controller)));
    registry.mark_tool_preapproved("agent").await;
    let spawned = registry
        .execute_tool(
            "agent",
            serde_json::json!({
                "action": "spawn_subprocess", "agent_type": "background-demo",
            }),
        )
        .await
        .expect("spawn through the session registry executor");
    let sibling = controller
        .background_status_entries(TaskOwner::Session(session_id.0.as_ref()))
        .await
        .into_iter()
        .next()
        .expect("sibling task");
    assert_eq!(sibling.owner_session_id.as_deref(), Some(session_id.0.as_ref()));
    assert_ne!(
        sibling.exec_session_id, shared_exec_session_id,
        "same-workspace sessions need distinct child processes"
    );
    assert_eq!(spawned["id"], sibling.id);
    let disabled = registry.with_subagent_controller(None);
    // Preapproval is consumed once, including on the successful spawn.
    disabled.mark_tool_preapproved("agent").await;
    let denied = disabled
        .execute_tool(
            "agent",
            serde_json::json!({
                "action": "spawn_subprocess", "agent_type": "background-demo",
            }),
        )
        .await
        .expect("registry execution errors are returned in the public JSON envelope");
    assert_eq!(denied["error"]["message"], "Subagent controller is not active in this session");
    assert_eq!(denied["error"]["category"], "ExecutionError");
    assert!(denied.get("id").is_none(), "an empty controller slot must not spawn a child: {denied}");
    assert!(registry.has_subagent_controller(), "detaching a sibling view must not clear its controller");
    sibling.id
}

/// Asserts every management request was accounted for and exported.
///
/// Request accounting is adapter telemetry recorded at the ACP transport
/// boundary, not domain state, so it is asserted here rather than folded into
/// the controller snapshot. The chosen contract counts every operation of the
/// management surface, so the `list` and `output` reads above are included in
/// the totals alongside the `cancel` commands. The collector's public export is
/// the production path an embedder reads.
pub(super) fn assert_management_requests_are_accounted(agent: &ZedAgent) {
    let metrics = agent.local_tool_registry.metrics_collector().get_acp_extension_metrics();
    assert!(
        metrics.management_requests_started >= 8,
        "every management request must be accounted for: {metrics:?}"
    );
    assert_eq!(
        metrics.management_requests_in_flight, 0,
        "every management request must leave the handler: {metrics:?}"
    );
    assert_eq!(
        metrics.management_requests_succeeded + metrics.management_requests_failed,
        metrics.management_requests_started,
        "every started request must record one outcome: {metrics:?}"
    );
    assert!(
        metrics.management_requests_failed >= 4,
        "the rejected cancel, oversized tail, and two unknown-task reads must record failures: {metrics:?}"
    );
    let prometheus = agent.local_tool_registry.metrics_collector().export_prometheus();
    let started_series =
        format!("diapason_acp_management_requests_started_total {}", metrics.management_requests_started);
    assert!(
        prometheus.lines().any(|line| line == started_series),
        "the public Prometheus export must render the ACP management counter: {started_series}"
    );
    let in_flight_series = "diapason_acp_management_requests_in_flight 0";
    assert!(
        prometheus.lines().any(|line| line == in_flight_series),
        "the public Prometheus export must render the ACP in-flight gauge: {in_flight_series}"
    );
}
