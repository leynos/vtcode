//! Pins that a terminal outcome observed by refresh reaches the ledger.
//!
//! Declared by `controller_background_settlement.rs`. Refresh is the only path
//! that sees an exec session end, and a record it settles becomes terminal: the
//! next pass skips it and restore filters it out. Without a write obligation the
//! durable row kept reporting a run that had ended, and a resumed session
//! restored a task that could never come back.

use tempfile::TempDir;

use crate::subagents::background::{load_background_state, persist_background_state};
use crate::tools::types::{DiapasonExecSession, DiapasonSessionLifecycleState};

use super::super::tests::support::test_controller_config;
use super::*;

const WORKER: &str = "background-worker";
const WORKER_AGENT: &str = "worker";
const LIVE_EXEC: &str = "exec-live";

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

/// One observation that ends a launch, as the refresh pass sees it.
#[derive(Clone, Copy)]
enum TerminalObservation {
    /// The exec session is readable and has exited with this code.
    Exited(i32),
    /// The exec session is gone.
    Missing,
}

impl TerminalObservation {
    /// The snapshot a refresh reads for this observation.
    fn snapshot(self) -> Option<DiapasonExecSession> {
        match self {
            Self::Exited(exit_code) => Some(exited_snapshot(exit_code)),
            Self::Missing => None,
        }
    }
}

/// Every terminal settlement of a refresh reaches the ledger.
///
/// Refresh is the only path that sees an exec session end, and a record it
/// settles becomes terminal: the next pass skips it and restore filters it out.
/// Nothing else ever looks at it again, so the outcome must be owed to the
/// ledger in the same lock that stores it; without that the durable row kept
/// reporting a run that had ended, and a resumed session restored a task that
/// could never come back.
///
/// The arms share one body because they differ only in the facts that decide
/// the outcome: whether the record is still wanted, the observation, and the
/// terminal state it settles. A change to the contract then cannot leave one
/// arm settling something the ledger never receives. The full-refresh entry
/// point is covered separately below.
#[rstest::rstest]
#[case::exited_while_desired(
    true,
    TerminalObservation::Exited(7),
    BackgroundSubprocessStatus::Error,
    Some("exited with code 7")
)]
#[case::exited_after_stop(false, TerminalObservation::Exited(0), BackgroundSubprocessStatus::Stopped, None)]
#[case::missing_while_desired(
    true,
    TerminalObservation::Missing,
    BackgroundSubprocessStatus::Error,
    Some("is not running")
)]
#[tokio::test]
async fn a_terminal_settlement_reaches_the_ledger(
    #[case] desired_enabled: bool,
    #[case] observation: TerminalObservation,
    #[case] expected_status: BackgroundSubprocessStatus,
    #[case] expected_error: Option<&str>,
) {
    let temp = TempDir::new().expect("tempdir");
    let mut record = running_record();
    record.desired_enabled = desired_enabled;
    persist_background_state(temp.path(), vec![record.clone().into_persisted()])
        .await
        .expect("seed the durable row");
    let config = test_controller_config(temp.path().to_path_buf());
    let controller = SubagentController::new(config).await.expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), record);
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the fixture starts with no pending write, so only the settlement can raise one"
    );

    controller
        .update_background_record_state_observed(
            WORKER,
            Some(LIVE_EXEC),
            Some(BackgroundSubprocessStatus::Running),
            observation.snapshot(),
        )
        .await
        .expect("an observed end is not a settlement failure");
    assert!(
        controller
            .retry_owed_background_ledger_write()
            .await
            .expect("the owed write succeeds"),
        "the raised obligation is what refresh retries, so the settlement must have raised one"
    );

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the settled record reaches the ledger");
    assert_eq!(stored.status, expected_status, "the ledger must report the outcome the settlement recorded");
    assert!(stored.ended_at.is_some(), "a terminal row carries its end instant");
    match expected_error {
        Some(expected) => assert!(
            stored.error.as_deref().is_some_and(|error| error.contains(expected)),
            "the durable row carries the reason the run ended: {:?}",
            stored.error
        ),
        None => assert_eq!(stored.error, None, "a completed stop carries no failure: {:?}", stored.error),
    }

    controller.signal_shutdown().await;
}

/// A refresh pass writes the terminal outcome it settles.
///
/// The settlement and retry helpers called directly cannot prove the entry
/// point does both. This starts from a durable `Running` row with nothing owed,
/// runs one refresh, and reads the ledger back: the row must be terminal and
/// carry its end instant, because nothing else will ever look at the record
/// again.
#[tokio::test]
async fn a_refresh_pass_persists_the_terminal_outcome_it_settles() {
    let temp = TempDir::new().expect("tempdir");
    persist_background_state(temp.path(), vec![running_record().into_persisted()])
        .await
        .expect("seed the durable live row");
    let config = test_controller_config(temp.path().to_path_buf());
    let controller = SubagentController::new(config).await.expect("controller");
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(WORKER.to_string(), running_record());
    }
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the fixture starts with no pending write, so only refresh can raise one"
    );

    controller
        .refresh_background_processes()
        .await
        .expect("the refresh pass completes");

    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|record| record.id == WORKER)
        .expect("the settled row reaches the ledger");
    assert_eq!(
        stored.status,
        BackgroundSubprocessStatus::Error,
        "the refresh writes the terminal outcome it settled"
    );
    assert!(stored.ended_at.is_some(), "a terminal row written by refresh carries its end instant");
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_none(),
        "the pass discharges the write its settlement owed"
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
    let config = test_controller_config(temp.path().to_path_buf());
    let controller = SubagentController::new(config).await.expect("controller");
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
