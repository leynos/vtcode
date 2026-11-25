//! Pins that a terminal outcome observed by refresh reaches the ledger.
//!
//! Declared by `controller_background_settlement.rs`. Refresh is the only path
//! that sees an exec session end, and a record it settles becomes terminal: the
//! next pass skips it and restore filters it out. Without a write obligation the
//! durable row kept reporting a run that had ended, and a resumed session
//! restored a task that could never come back.

use std::path::PathBuf;

use tempfile::TempDir;

use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::background::{load_background_state, persist_background_state};
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;
use crate::tools::types::{DiapasonExecSession, DiapasonSessionLifecycleState};

use super::*;

const WORKER: &str = "background-worker";
const WORKER_AGENT: &str = "worker";
const LIVE_EXEC: &str = "exec-live";

fn test_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let vt_cfg = DiapasonConfig::default();
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), vt_cfg.pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: "gpt-5.6-sol".to_string(),
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
    }
}

/// The durable row a refresh starts from: a live launch with no owed write.
fn running_record() -> BackgroundRecord {
    BackgroundRecord {
        id: WORKER.to_string(),
        owner_session_id: None,
        agent_name: WORKER_AGENT.to_string(),
        display_label: WORKER_AGENT.to_string(),
        description: WORKER_AGENT.to_string(),
        source: "builtin".to_string(),
        colour: None,
        session_id: format!("session-{WORKER}"),
        parent_session_id: "parent-session".to_string(),
        exec_session_id: LIVE_EXEC.to_string(),
        desired_enabled: true,
        status: BackgroundSubprocessStatus::Running,
        created_at: "2026-08-28T00:00:00Z".parse().expect("created_at"),
        updated_at: "2026-08-28T00:00:00Z".parse().expect("updated_at"),
        started_at: Some("2026-08-28T00:00:00Z".parse().expect("started_at")),
        ended_at: None,
        pid: None,
        prompt: "work".to_string(),
        summary: None,
        error: None,
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 1,
    }
}

/// An exited session settles the record and the settlement is written.
///
/// This is the case refresh exists for: the process ended while nobody was
/// watching. The single restart allowance is already spent, so the record
/// becomes `Error` with an end instant. Nothing else ever looks at it again, so
/// the outcome must be owed to the ledger in the same lock that stores it.
#[tokio::test]
async fn an_exited_launch_settles_error_and_reaches_the_ledger() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![running_record().into_persisted()])
        .await
        .expect("seed the durable live row");
    let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), running_record());
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the fixture starts with no pending write, so only the settlement can raise one"
    );

    let exited = exited_snapshot(7);
    controller
        .update_background_record_state_observed(
            WORKER,
            Some(LIVE_EXEC),
            Some(BackgroundSubprocessStatus::Running),
            Some(exited),
        )
        .await
        .expect("an exit is not a settlement failure");

    let owed = pending_background_ledger_write_generation(&controller.state);
    assert!(owed.is_some(), "the terminal settlement must owe the ledger the outcome it recorded");
    assert!(
        controller
            .retry_owed_background_ledger_write()
            .await
            .expect("the owed write succeeds"),
        "the raised obligation is what refresh retries"
    );

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the settled record reaches the ledger");
    assert_eq!(
        stored.status,
        BackgroundSubprocessStatus::Error,
        "the ledger must report the run that ended, not the live row it replaced"
    );
    assert!(stored.ended_at.is_some(), "a terminal row carries its end instant");

    controller.signal_shutdown().await;
}

/// A stopped launch observed as exited settles `Stopped` and is written.
///
/// The other terminal arm: the record was already no longer desired, so the
/// exit is its successful completion. It is skipped by every later pass in
/// exactly the same way, so it needs the same obligation.
#[tokio::test]
async fn an_exited_launch_settles_stopped_and_reaches_the_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let mut record = running_record();
    record.desired_enabled = false;
    persist_background_state(temp.path(), vec![record.clone().into_persisted()])
        .await
        .expect("seed the durable row");
    let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }

    controller
        .update_background_record_state_observed(
            WORKER,
            Some(LIVE_EXEC),
            Some(BackgroundSubprocessStatus::Running),
            Some(exited_snapshot(0)),
        )
        .await
        .expect("an exit is not a settlement failure");
    assert!(
        controller
            .retry_owed_background_ledger_write()
            .await
            .expect("the owed write succeeds"),
        "the stopped settlement must be owed to the ledger"
    );

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the settled record reaches the ledger");
    assert_eq!(stored.status, BackgroundSubprocessStatus::Stopped);
    assert!(stored.ended_at.is_some(), "a stopped row carries its end instant");

    controller.signal_shutdown().await;
}

/// A missing session with no retry left settles `Error` and is written.
///
/// The missing-session arm reaches a terminal status too, and for the same
/// reason: refresh is the only observer, and it skips the record afterwards.
#[tokio::test]
async fn a_missing_launch_settles_error_and_reaches_the_ledger() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![running_record().into_persisted()])
        .await
        .expect("seed the durable live row");
    let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), running_record());
    }

    controller
        .update_background_record_state_observed(WORKER, Some(LIVE_EXEC), None, None)
        .await
        .expect("a missing session is not a settlement failure");
    assert!(
        controller
            .retry_owed_background_ledger_write()
            .await
            .expect("the owed write succeeds"),
        "the missing-session settlement must be owed to the ledger"
    );

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the settled record reaches the ledger");
    assert_eq!(stored.status, BackgroundSubprocessStatus::Error);
    assert!(
        stored.error.as_deref().is_some_and(|error| error.contains("is not running")),
        "the durable row carries the reason the run ended: {:?}",
        stored.error
    );

    controller.signal_shutdown().await;
}

/// A live row refresh left unchanged is not marked as owing a write.
///
/// The obligation must follow a change of outcome, not merely a refresh: an
/// unchanged live row is already durable, and marking it owed would make every
/// poll of a running task rewrite the ledger.
#[tokio::test]
async fn an_unchanged_live_row_owes_no_write() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![running_record().into_persisted()])
        .await
        .expect("seed the durable live row");
    let controller = SubagentController::new(test_config(temp.path().to_path_buf()))
        .await
        .expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), running_record());
    }

    controller
        .update_background_record_state_observed(
            WORKER,
            Some(LIVE_EXEC),
            Some(BackgroundSubprocessStatus::Running),
            Some(live_snapshot()),
        )
        .await
        .expect("a live observation is not a settlement failure");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "a live row that did not change must not owe a write"
    );

    controller.signal_shutdown().await;
}

fn exited_snapshot(exit_code: i32) -> DiapasonExecSession {
    let mut snapshot = live_snapshot();
    snapshot.lifecycle_state = Some(DiapasonSessionLifecycleState::Exited);
    snapshot.exit_code = Some(exit_code);
    snapshot
}

fn live_snapshot() -> DiapasonExecSession {
    DiapasonExecSession {
        id: LIVE_EXEC.to_string().into(),
        backend: "pipe".to_string(),
        command: "true".to_string(),
        args: Vec::new(),
        working_dir: None,
        rows: None,
        cols: None,
        child_pid: None,
        started_at: Some("2026-08-28T00:00:00Z".parse().expect("started_at")),
        lifecycle_state: Some(DiapasonSessionLifecycleState::Running),
        exit_code: None,
    }
}
