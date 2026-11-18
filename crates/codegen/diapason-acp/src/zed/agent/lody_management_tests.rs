//! Exercises Lody subagent management: liveness-projected listing and the
//! output-tail transport bound.

use super::*;
use assert_fs::TempDir;
use diapason_config::DiapasonConfig;
use diapason_core::config::types::ReasoningEffortLevel;
use diapason_core::subagents::SubagentControllerConfig;
use diapason_core::tools::exec_session::ExecSessionManager;
use diapason_core::tools::registry::PtySessionManager;

#[path = "lody_management_metrics_tests.rs"]
mod lody_management_metrics_tests;
#[path = "lody_management_privacy_tests.rs"]
mod lody_management_privacy_tests;

use lody_management_metrics_tests::acp_extension_metrics;

fn persisted_running_record_json(exec_session_id: &str) -> Value {
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
            "exec_session_id": exec_session_id,
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
    std::fs::write(
        state_dir.join("background_subagents.json"),
        persisted_running_record_json("exec-missing").to_string(),
    )
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
                lock_scope: None,
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

/// Builds an agent whose launch scope has a subagent controller.
///
/// A session without a workspace runtime resolves to that controller, which is
/// the controller a closing session would otherwise reach.
///
/// The setup future is boxed: it holds a whole default configuration and the
/// attach path's own future, so leaving it inline would make every awaiting
/// test's future large enough to trip `clippy::large_futures`.
fn agent_with_launch_subagent_controller(workspace: &std::path::Path) -> impl Future<Output = ZedAgent> + '_ {
    Box::pin(async move {
        let mut agent = crate::zed::tests::build_agent(workspace).await;
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
            "the test needs a launch-scope subagent controller"
        );
        agent
    })
}

/// A closing session's workspace runtime is released while it is still
/// resolvable, so controller resolution falls back to launch scope. Management
/// must not serve that scope for it: the request is rejected exactly like an
/// unknown session.
#[tokio::test]
async fn closing_session_rejects_management_requests_as_unknown_sessions() {
    let workspace = TempDir::new().expect("closing session workspace");
    let agent = agent_with_launch_subagent_controller(workspace.path()).await;
    let session_id = agent.register_session().expect("register session");
    let raw_id = session_id.0.to_string();
    // An open session still resolves: the launch scope is a legitimate
    // fallback while the session is live.
    assert!(session_controller(&agent, &raw_id).is_ok(), "an open session must still resolve its controller");

    agent
        .session_handle(&session_id)
        .expect("look up the session")
        .expect("registered session")
        .begin_close();

    let resolve_error = match session_controller(&agent, &raw_id) {
        Ok(_) => panic!("a closing session must not resolve a controller"),
        Err(error) => error,
    };
    assert_eq!(
        resolve_error.data.as_ref(),
        Some(&serde_json::json!({ "reason": "unknown_session" })),
        "a closing session must be reported as unknown: {resolve_error:?}"
    );
    // The dispatch boundary reports the same bounded reason and counts the
    // rejected request as a failure rather than a success.
    let before = acp_extension_metrics(&agent);
    let error = handle_lody_subagent_management(
        &agent,
        crate::zed::agent::lody::LODY_SUBAGENTS_LIST_METHOD,
        serde_json::json!({ "sessionId": raw_id }),
    )
    .await
    .expect_err("a closing session cannot list tasks");
    assert_eq!(
        error.data.as_ref(),
        Some(&serde_json::json!({ "reason": "unknown_session" })),
        "the dispatch boundary must not expose launch-scope tasks: {error:?}"
    );
    let after = acp_extension_metrics(&agent);
    assert_eq!(
        after.management_requests_started,
        before.management_requests_started + 1,
        "a rejected request is still a management request: {after:?}"
    );
    assert_eq!(
        after.management_requests_failed,
        before.management_requests_failed + 1,
        "a rejected request is recorded as a failure: {after:?}"
    );
}

/// A close that starts during resolution is rejected as well.
///
/// The request can pass the first closing check and then wait while the session
/// closes and releases its workspace runtime. Resolution then falls back to the
/// launch-scope controller, so the second check has to reject the interleaving
/// before that controller is used. The closure closes the session exactly where
/// the release would happen.
#[tokio::test]
async fn close_during_resolution_rejects_the_resolved_controller() {
    let workspace = TempDir::new().expect("close-during-resolution workspace");
    let agent = agent_with_launch_subagent_controller(workspace.path()).await;
    let session_id = agent.register_session().expect("register session");
    let session = agent
        .session_handle(&session_id)
        .expect("look up the session")
        .expect("registered session");
    let launch_scope = agent
        .session_subagent_controller(&session)
        .expect("resolve the launch scope before closing")
        .expect("the launch scope is configured");

    let resolved = resolve_open_session_controller(&session, || {
        session.begin_close();
        Ok(Some(launch_scope))
    });

    let error = match resolved {
        Ok(_) => panic!("a close during resolution must not yield a controller"),
        Err(error) => error,
    };
    assert_eq!(
        error.data.as_ref(),
        Some(&serde_json::json!({ "reason": "unknown_session" })),
        "the interleaving must report the same bounded reason: {error:?}"
    );
}

/// A close during resolution also outranks a resolution that found nothing.
///
/// Releasing the workspace runtime can leave the resolver with no controller,
/// and reporting that as unavailable would tell the client to retry a session
/// that is already gone. The closing check therefore has to run before the
/// resolver's empty result is interpreted.
#[tokio::test]
async fn close_during_resolution_outranks_an_empty_resolution() {
    let workspace = TempDir::new().expect("close-during-resolution workspace");
    let agent = agent_with_launch_subagent_controller(workspace.path()).await;
    let session_id = agent.register_session().expect("register session");
    let session = agent
        .session_handle(&session_id)
        .expect("look up the session")
        .expect("registered session");

    let resolved = resolve_open_session_controller(&session, || {
        session.begin_close();
        Ok(None)
    });

    let error = match resolved {
        Ok(_) => panic!("a close during resolution must not yield a controller"),
        Err(error) => error,
    };
    assert_eq!(
        error.data.as_ref(),
        Some(&serde_json::json!({ "reason": "unknown_session" })),
        "an empty resolution during a close must not be reported as unavailable: {error:?}"
    );
}

/// A close during resolution likewise outranks the resolver's own failure.
///
/// The resolver can surface an internal error while the session is closing.
/// The client cannot act on that error, and retrying the session would be
/// wrong, so the bounded `unknown_session` reason wins over it.
#[tokio::test]
async fn close_during_resolution_outranks_a_resolver_error() {
    let workspace = TempDir::new().expect("close-during-resolution workspace");
    let agent = agent_with_launch_subagent_controller(workspace.path()).await;
    let session_id = agent.register_session().expect("register session");
    let session = agent
        .session_handle(&session_id)
        .expect("look up the session")
        .expect("registered session");

    let resolved = resolve_open_session_controller(&session, || {
        session.begin_close();
        Err(internal_management_error(anyhow::anyhow!("controller unavailable during close")))
    });

    let error = match resolved {
        Ok(_) => panic!("a close during resolution must not yield a controller"),
        Err(error) => error,
    };
    assert_eq!(
        error.data.as_ref(),
        Some(&serde_json::json!({ "reason": "unknown_session" })),
        "a resolver failure during a close must not leak an internal error: {error:?}"
    );
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
