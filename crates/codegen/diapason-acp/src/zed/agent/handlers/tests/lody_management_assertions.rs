//! Management-surface assertions for the Lody ACP extensions.
use super::lody_support::{expected_epoch_seconds, lody_cancel, lody_list, lody_output};
use super::*;
use diapason_core::subagents::{SubagentController, TaskOwner};
use std::time::Duration;

/// Spawns through a detached session registry view.
///
/// The route must execute through the session's own controller and never fall
/// back to the shared runtime's ownerless controller once the view is detached.
/// Returns the id of the sibling task the session-owned route created.
pub(super) async fn exercise_detached_session_registry(
    runtime: &crate::zed::agent::SessionWorkspaceRuntime,
    controller: &Arc<SubagentController>,
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

/// The management requests one fixture issued, counted by outcome.
///
/// The fixture counts what it actually sent, including every output poll, so
/// the collector has to match it exactly: a request the adapter stops counting
/// cannot hide behind a minimum total.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ManagementRequestTotals {
    pub(super) succeeded: u64,
    pub(super) failed: u64,
}

impl ManagementRequestTotals {
    /// Every request the fixture sent, whatever its outcome.
    fn started(self) -> u64 {
        self.succeeded.saturating_add(self.failed)
    }
}

/// Asserts every management request the fixture sent was accounted for and
/// exported.
///
/// Request accounting is adapter telemetry recorded at the ACP transport
/// boundary, not domain state, so it is asserted here rather than folded into
/// the controller snapshot. The chosen contract counts every operation of the
/// management surface, so the `list` and `output` reads are included in the
/// totals alongside the `cancel` commands. The collector's public export is the
/// production path an embedder reads.
pub(super) fn assert_management_requests_are_accounted(agent: &ZedAgent, expected: ManagementRequestTotals) {
    let metrics = agent.local_tool_registry.metrics_collector().get_acp_extension_metrics();
    assert_eq!(
        metrics.management_requests_started,
        expected.started(),
        "every management request the fixture sent must be accounted for: {metrics:?}"
    );
    assert_eq!(
        metrics.management_requests_succeeded, expected.succeeded,
        "every succeeded management request must be counted: {metrics:?}"
    );
    assert_eq!(
        metrics.management_requests_failed, expected.failed,
        "every failed management request must be counted: {metrics:?}"
    );
    assert_eq!(
        metrics.management_requests_in_flight, 0,
        "every management request must leave the handler: {metrics:?}"
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

/// Stops a background task and asserts its exec session close was confirmed.
///
/// A stop is complete only once the runtime confirms the exec session closed.
/// `ClosePending` means the record settled while its process may still be live,
/// so treating it as a settled task would hide exactly the leak test cleanup
/// exists to remove.
pub(super) async fn cancel_background_task(
    controller: &SubagentController,
    owner: TaskOwner<'_>,
    task_id: &str,
) -> diapason_core::subagents::BackgroundSubprocessEntry {
    let outcome = controller
        .force_cancel_background(owner, task_id)
        .await
        .expect("cancel a background task during test cleanup");
    assert!(
        !outcome.is_close_pending(),
        "a stop must confirm its exec session closed before the test treats the task as stopped: {outcome:?}"
    );
    outcome.into_entry()
}

/// Asserts a sibling session can neither see nor address another session's
/// tree.
///
/// A foreign task must be indistinguishable from an unknown one on both
/// management methods, so ownership is not an oracle.
pub(super) async fn assert_sibling_cannot_reach_the_tree(
    cx: &ConnectionTo<Agent>,
    owner_session_id: &acp::SessionId,
    foreign_session_id: &acp::SessionId,
    grandchild_id: &str,
    child_controller: &Arc<SubagentController>,
) -> Result<(), acp::Error> {
    let foreign_list = lody_list(cx, foreign_session_id, false).await?;
    assert_eq!(foreign_list, serde_json::json!({ "tasks": [] }));

    let foreign_output = lody_output(cx, foreign_session_id, grandchild_id)
        .await
        .expect_err("a sibling must not read a grandchild's output");
    let unknown_output = lody_output(cx, owner_session_id, "task-that-does-not-exist")
        .await
        .expect_err("an unknown task ID must not resolve");
    assert_eq!(
        foreign_output, unknown_output,
        "a foreign output request must return the same error value as an unknown one"
    );
    assert_eq!(foreign_output.data, Some(serde_json::json!({ "reason": "unknown_task" })));

    let foreign_cancel = lody_cancel(cx, foreign_session_id, grandchild_id)
        .await
        .expect_err("a sibling must not cancel a grandchild");
    let unknown_cancel = lody_cancel(cx, owner_session_id, "task-that-does-not-exist")
        .await
        .expect_err("cancelling an unknown task must fail");
    assert_eq!(
        foreign_cancel, unknown_cancel,
        "a foreign cancel must return the same error value as an unknown one"
    );
    assert_eq!(foreign_cancel.data, Some(serde_json::json!({ "reason": "unknown_task" })));
    assert!(
        !child_controller
            .status_entries()
            .await
            .into_iter()
            .find(|entry| entry.id == grandchild_id)
            .expect("grandchild record")
            .status
            .is_terminal(),
        "a rejected sibling cancel must not stop the grandchild"
    );
    Ok(())
}

/// Asserts the owning session reads and cancels exactly one owned grandchild.
///
/// The read must be served through the child-scoped controller that spawned the
/// grandchild, and cancelling it must settle that record alone: the unfiltered
/// list keeps it with its end instant, `activeOnly` drops it, and its running
/// sibling and parent are untouched.
pub(super) async fn assert_owned_grandchild_cancel_settles_only_it(
    cx: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    root: &Arc<SubagentController>,
    child_controller: &Arc<SubagentController>,
    child_id: &str,
    grandchild_id: &str,
    second_grandchild_id: &str,
) -> Result<(), acp::Error> {
    // The runtime snapshot materializes once the child run has recorded its
    // configuration, so poll briefly.
    let owned_output = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(output) = lody_output(cx, session_id, grandchild_id).await {
                break output;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the owning session must read the grandchild's output");
    assert!(
        owned_output.get("output").is_some(),
        "grandchild output must use the managed output envelope: {owned_output}"
    );

    // The owner cancels one grandchild directly: management must route through
    // the child-scoped controller that owns it, not merely through the root the
    // task was listed from.
    let cancelled_grandchild = lody_cancel(cx, session_id, grandchild_id)
        .await
        .expect("the owning session must cancel an owned grandchild");
    assert_eq!(cancelled_grandchild, serde_json::json!({}));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let terminal = child_controller
                .status_entries()
                .await
                .into_iter()
                .find(|entry| entry.id == grandchild_id)
                .is_some_and(|entry| entry.status.is_terminal());
            if terminal {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelling an owned grandchild must settle it directly");

    let unfiltered_after_cancel = lody_list(cx, session_id, false).await?;
    assert_eq!(
        unfiltered_after_cancel["tasks"].as_array().map(Vec::len),
        Some(3),
        "the unfiltered list must keep a terminal task: {unfiltered_after_cancel}"
    );
    let cancelled_record = child_controller
        .status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == grandchild_id)
        .expect("the cancelled grandchild record");
    let cancelled_listed = unfiltered_after_cancel["tasks"]
        .as_array()
        .expect("Lody task list")
        .iter()
        .find(|task| task["taskId"].as_str() == Some(grandchild_id))
        .expect("the cancelled grandchild must remain listed");
    assert_eq!(
        cancelled_listed["status"], "killed",
        "a cancelled foreground task is reported as killed: {cancelled_listed}"
    );
    assert_eq!(
        cancelled_listed["endedAtEpochSeconds"],
        expected_epoch_seconds(cancelled_record.completed_at.expect("a cancelled task records its completion")),
        "the listed end must project the owning controller record: {cancelled_listed}"
    );
    let active_after_cancel = lody_list(cx, session_id, true).await?;
    let mut active_after_cancel_ids = active_after_cancel["tasks"]
        .as_array()
        .expect("Lody task list")
        .iter()
        .map(|task| task["taskId"].as_str().expect("every listed task must carry a string task ID"))
        .collect::<Vec<_>>();
    active_after_cancel_ids.sort_unstable();
    let mut expected_active_ids = vec![child_id, second_grandchild_id];
    expected_active_ids.sort_unstable();
    assert_eq!(
        active_after_cancel_ids, expected_active_ids,
        "activeOnly must exclude the cancelled grandchild but keep its running siblings: {active_after_cancel}"
    );

    let sibling_still_running = child_controller
        .status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == second_grandchild_id)
        .is_some_and(|entry| !entry.status.is_terminal());
    assert!(sibling_still_running, "cancelling one grandchild must not cancel its sibling");
    let child_still_running = root
        .status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == child_id)
        .is_some_and(|entry| !entry.status.is_terminal());
    assert!(child_still_running, "cancelling a grandchild must not cancel its parent");
    Ok(())
}

/// Asserts closing the child closes the rest of its subtree.
pub(super) async fn assert_closing_child_closes_its_subtree(
    cx: &ConnectionTo<Agent>,
    session_id: &acp::SessionId,
    child_controller: &Arc<SubagentController>,
    child_id: &str,
    second_grandchild_id: &str,
) -> Result<(), acp::Error> {
    let cancelled = lody_cancel(cx, session_id, child_id)
        .await
        .expect("the owning session must cancel its child");
    assert_eq!(cancelled, serde_json::json!({}));
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let terminal = child_controller
                .status_entries()
                .await
                .into_iter()
                .find(|entry| entry.id == second_grandchild_id)
                .is_some_and(|entry| entry.status.is_terminal());
            if terminal {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("closing the child must close its remaining grandchild subtree");

    let unfiltered_after_close = lody_list(cx, session_id, false).await?;
    assert_eq!(
        unfiltered_after_close["tasks"].as_array().map(Vec::len),
        Some(3),
        "closing the subtree must not remove its tasks from the unfiltered list: {unfiltered_after_close}"
    );
    let active_after_close = lody_list(cx, session_id, true).await?;
    assert_eq!(
        active_after_close,
        serde_json::json!({ "tasks": [] }),
        "activeOnly must exclude every terminal task"
    );
    Ok(())
}
