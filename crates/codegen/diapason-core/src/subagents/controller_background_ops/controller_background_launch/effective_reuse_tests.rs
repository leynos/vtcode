//! Pins the effective-settings rule the reuse check compares, the parent a
//! reuse may answer for, and the ledger obligation reuse waits for.
//!
//! Declared by `controller_background_launch.rs`. The fixtures live here so the
//! production module keeps only the launch entry point and its reuse logic.

use tempfile::TempDir;

use crate::subagents::background::{background_state_lock_path_for_tests, load_background_state};

use super::super::controller_background_settlement::{
    mark_background_ledger_write_pending, pending_background_ledger_write_generation,
};
use super::super::tests::support::test_controller_config;
use super::*;

fn background_record(model_override: Option<&str>) -> BackgroundRecord {
    BackgroundRecord {
        id: "background-worker".to_string(),
        owner_session_id: None,
        agent_name: "worker".to_string(),
        display_label: "worker".to_string(),
        description: "worker".to_string(),
        source: "builtin".to_string(),
        colour: None,
        session_id: "session-background-worker".to_string(),
        parent_session_id: "parent-session".to_string(),
        exec_session_id: "exec-live".to_string(),
        desired_enabled: true,
        status: BackgroundSubprocessStatus::Running,
        created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
        updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
        started_at: None,
        ended_at: None,
        pid: None,
        prompt: "work".to_string(),
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: model_override.map(str::to_owned),
        reasoning_override: None,
        restart_attempts: 0,
    }
}

/// Builds the request half of a launch whose spec defaults to `spec_model`.
fn prepared_launch(request_model: Option<&str>, spec_model: Option<&str>) -> PreparedBackgroundSubprocess {
    PreparedBackgroundSubprocess {
        agent_name: "worker".to_string(),
        record_id: "background-worker".to_string(),
        prompt: "work".to_string(),
        desired_max_turns: None,
        desired_model_override: spec_model.map(str::to_owned),
        desired_reasoning_override: None,
        max_turns: None,
        model_override: request_model.map(str::to_owned),
        reasoning_override: None,
        parent_session_id: "parent-session".to_string(),
    }
}

async fn active_record_controller(workspace: &TempDir, model_override: Option<&str>) -> SubagentController {
    let config = test_controller_config(workspace.path().to_path_buf());
    let controller = SubagentController::new(config).await.expect("controller");
    let record = background_record(model_override);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    controller
}

/// A request that omits a setting must use the record's value, as the plan
/// does, instead of conflicting with the spec's default.
///
/// The record carries model X while the spec defaults to model Y. A request
/// without a model resolves to X through the plan's
/// request-then-record-then-spec precedence, so the active launch is
/// compatible and must be reused rather than rejected as a different model.
#[tokio::test]
async fn a_request_without_a_model_reuses_the_active_records_model() {
    let temp = TempDir::new().expect("tempdir");
    let controller = active_record_controller(&temp, Some("model-x")).await;

    let reused = controller
        .reusable_background_subprocess(&prepared_launch(None, Some("model-y")))
        .await
        .expect("a request that omits the model reuses the active record");

    let entry = reused.expect("the active launch satisfies the request");
    assert_eq!(entry.id, "background-worker");
    assert_eq!(entry.exec_session_id, "exec-live", "reuse must not replace the running launch");

    controller.signal_shutdown().await;
}

/// An explicit request for a different model is still a conflict.
///
/// Only an omitted setting falls back to the record; an explicit request
/// outranks it, exactly as in the plan.
#[tokio::test]
async fn an_explicitly_different_model_still_conflicts() {
    let temp = TempDir::new().expect("tempdir");
    let controller = active_record_controller(&temp, Some("model-x")).await;

    let error = controller
        .reusable_background_subprocess(&prepared_launch(Some("model-z"), Some("model-z")))
        .await
        .expect_err("a request for different work must not reuse the active record");

    assert!(error.to_string().contains("different model"), "the conflict names the model: {error:#}");

    controller.signal_shutdown().await;
}

/// A launch another parent session drives is not reusable before the lock.
///
/// The pre-lock reuse check compared only the ambient invocation context, so a
/// caller outside a tool invocation passed it. Answering with the record would
/// then route its progress to the other parent while the caller is told its own
/// request succeeded, so the resolved parent is compared as well.
#[tokio::test]
async fn a_reuse_for_another_parent_session_is_refused_before_the_lock() {
    let temp = TempDir::new().expect("tempdir");
    let controller = active_record_controller(&temp, Some("model-x")).await;
    {
        let mut state = controller.state.write().await;
        let record = state
            .background_children
            .get_mut("background-worker")
            .expect("the active record");
        record.parent_session_id = "other-session".to_string();
    }

    let error = controller
        .reusable_background_subprocess(&prepared_launch(None, Some("model-x")))
        .await
        .expect_err("another parent's launch must not be reused");
    assert!(
        format!("{error:#}").contains("already active"),
        "the refusal uses the active-launch rule: {error:#}"
    );

    controller.signal_shutdown().await;
}

/// Reuse must not answer from memory while the record's write is owed.
///
/// An explicit request can finish `refresh_background_processes` just
/// before a matching launch commits `Running`. This lookup would then
/// report that record as durable although its ledger write may still fail.
/// The selection checks the pending-write marker under its own lock, retries
/// the write, and answers only once the record is durable.
#[tokio::test]
async fn a_reuse_waits_for_the_active_records_owed_ledger_write() {
    let temp = TempDir::new().expect("tempdir");
    let controller = active_record_controller(&temp, Some("model-x")).await;
    let generation = mark_background_ledger_write_pending(&controller.state);

    // An unacquirable ledger lock makes the retry fail, so reuse must report
    // that fault rather than answering from the in-memory record.
    let lock_path = background_state_lock_path_for_tests(temp.path());
    std::fs::create_dir_all(&lock_path).expect("the lock path blocks the write");
    let error = controller
        .reusable_background_subprocess(&prepared_launch(None, Some("model-x")))
        .await
        .expect_err("reuse must retry the owed write before answering");
    assert!(
        format!("{error:#}").contains("background_subagents.lock"),
        "the reuse fault names the ledger lock: {error:#}"
    );
    assert_eq!(
        pending_background_ledger_write_generation(&controller.state),
        Some(generation),
        "a failed retry leaves the obligation owed"
    );
    std::fs::remove_dir(&lock_path).expect("the lock path is writable again");

    let reused = controller
        .reusable_background_subprocess(&prepared_launch(None, Some("model-x")))
        .await
        .expect("reuse waits for the owed write");
    let entry = reused.expect("the active launch satisfies the request");
    assert_eq!(entry.exec_session_id, "exec-live");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "a successful write discharges the obligation"
    );
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == "background-worker")
        .expect("the record reuse reported is durable");
    assert_eq!(stored.exec_session_id, "exec-live", "the reused record reached the ledger");

    controller.signal_shutdown().await;
}

/// A parentless record is left to the rebinding reservation inside a tool invocation.
///
/// The pre-lock selector cannot rebind the record - that needs the write lock -
/// but it must not refuse it either: the ambient context names the session the
/// tool call belongs to, and a record that names no parent has no owner to
/// conflict with. Returning "already active" here made the rebind unreachable
/// for every tool caller.
#[tokio::test]
async fn a_parentless_record_is_left_to_the_rebinding_reservation_inside_a_tool_invocation() {
    let temp = TempDir::new().expect("tempdir");
    let controller = active_record_controller(&temp, None).await;
    {
        let mut state = controller.state.write().await;
        let record = state
            .background_children
            .get_mut("background-worker")
            .expect("the active record");
        record.parent_session_id = String::new();
    }
    let context = SubagentInvocationContext::new("parent-session".to_string(), Vec::new(), String::new());

    let reused = with_subagent_invocation_context(
        context,
        controller.reusable_background_subprocess(&prepared_launch(None, None)),
    )
    .await
    .expect("a tool invocation must not be refused a parentless record");

    assert!(
        reused.is_none(),
        "the pre-lock selector cannot rebind the record, so it leaves it to the reservation"
    );

    controller.signal_shutdown().await;
}
