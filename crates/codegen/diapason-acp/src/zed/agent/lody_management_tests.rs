//! Exercises Lody subagent management: liveness-projected listing and the
//! output-tail transport bound.

use super::*;
use assert_fs::TempDir;
use diapason_config::DiapasonConfig;
use diapason_core::config::types::ReasoningEffortLevel;
use diapason_core::subagents::SubagentControllerConfig;
use diapason_core::tools::exec_session::ExecSessionManager;
use diapason_core::tools::registry::PtySessionManager;

fn persisted_running_record_json() -> Value {
    serde_json::json!({
        "records": [{
            "id": "background-worker",
            "agent_name": "worker",
            "display_label": "worker",
            "description": "worker",
            "source": "builtin",
            "color": null,
            "owner_session_id": "session-a",
            "session_id": "session-worker",
            "exec_session_id": "exec-missing",
            "desired_enabled": false,
            "status": "running",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": "2026-08-28T00:00:00Z",
            "ended_at": null,
            "pid": 4321,
            "prompt": "work",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    })
}

/// `_lody/subagents/list` is a read-only query: it projects cached records
/// with liveness overlaid, so an exited process is not reported as running
/// and a read never mutates the record set or rewrites the ledger.
#[tokio::test]
async fn listing_projects_liveness_without_mutating_records() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".diapason/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    std::fs::write(state_dir.join("background_subagents.json"), persisted_running_record_json().to_string())
        .expect("write state");

    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(temp.path().to_path_buf(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(temp.path().to_path_buf(), pty_sessions.clone());
    let controller = Arc::new(
        SubagentController::new_with_background_owner(
            SubagentControllerConfig {
                workspace_root: temp.path().to_path_buf(),
                parent_session_id: "session-a".to_string(),
                parent_model: "test-model".to_string(),
                parent_provider: "openai".to_string(),
                parent_reasoning_effort: ReasoningEffortLevel::Medium,
                api_key: "test-key".to_string(),
                vt_cfg,
                openai_chatgpt_auth: None,
                depth: 0,
                workspace_gated: false,
                exec_sessions,
                output_reader: None,
                pty_manager: pty_sessions.manager().clone(),
                managed_background_runtime: false,
            },
            Some("session-a"),
        )
        .await
        .expect("scoped controller"),
    );
    let controllers = [Arc::clone(&controller)];

    let cached = background_status_entries(&controllers).await;
    assert_eq!(
        cached.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the cached projection reports the persisted state"
    );

    let ledger_path = state_dir.join("background_subagents.json");
    let ledger_before = std::fs::read_to_string(&ledger_path).expect("read ledger before list");

    let observed = observed_background_entries(&controllers).await;
    assert_eq!(
        observed.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Stopped],
        "listing must observe the exit instead of reporting a stale running process"
    );

    let after = background_status_entries(&controllers).await;
    assert_eq!(
        after.iter().map(|entry| entry.status).collect::<Vec<_>>(),
        [BackgroundSubprocessStatus::Running],
        "the read-only query must not mutate the record set"
    );

    let ledger_after = std::fs::read_to_string(&ledger_path).expect("read ledger after list");
    assert_eq!(ledger_before, ledger_after, "the read-only query must not rewrite the persisted ledger");
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
    let mut agent = crate::zed::tests::build_agent(workspace.path()).await;
    let mut vt_config = DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    agent.vt_config = Some(Box::new(vt_config.clone()));
    let outcome = crate::zed::agent::optional_setup::attach_acp_subagent_controller(
        crate::zed::agent::AcpSubagentControllerSetup {
            registry: &agent.local_tool_registry,
            agent_config: &agent.config,
            custom_providers: &[],
            vt_config: Some(&vt_config),
            owner_session_id: None,
            workspace_gated: false,
            defer_background_restore: false,
        },
    )
    .await;
    assert_eq!(
        outcome,
        crate::zed::agent::OptionalSetupOutcome::Attached,
        "the dispatch-boundary test needs a session subagent controller"
    );

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
fn acp_extension_metrics(agent: &ZedAgent) -> diapason_core::metrics::AcpExtensionMetrics {
    agent.local_tool_registry.metrics_collector().get_acp_extension_metrics()
}

/// The output-tail bound is an ACP transport contract, so the adapter owns
/// the default and the hard maximum rather than the domain controller.
///
/// These are the ACP transport contract literals: an omitted `tail` means
/// 200 lines, 10_000 is the inclusive maximum, and 10_001 is rejected.
#[test]
fn output_tail_defaults_and_rejects_values_above_the_transport_bound() {
    assert_eq!(normalize_output_tail_lines(None).expect("default tail size"), 200);
    assert_eq!(normalize_output_tail_lines(Some(10_000)).expect("boundary tail size"), 10_000);
    let error =
        normalize_output_tail_lines(Some(10_001)).expect_err("a tail above the transport bound must be rejected");
    assert!(error.contains("10000"), "the rejection must name the bound: {error}");
}
