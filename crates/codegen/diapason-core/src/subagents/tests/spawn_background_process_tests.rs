//! Managed background subprocess spawn guards.
use super::*;
#[cfg(unix)]
use rstest::rstest;

/// The exec session a reused record names in these tests.
#[cfg(unix)]
const LIVE_BACKGROUND_SESSION: &str = "exec-session-background-demo";

/// Builds a controller with an active record whose exec session is alive.
///
/// The pre-spawn refresh settles a record whose exec session it cannot observe,
/// so the reuse path only runs when the session is really alive. The session is
/// a `cat` process blocked on its stdin: it stays alive until the fixture closes
/// it, so a slow machine cannot settle the record `Error` before the reuse check
/// runs. A `sleep` command would race that refresh and make the tests flaky.
///
/// The caller owns both the controller and the session manager and must close
/// the session, even when an assertion fails.
///
/// Unix only: the fixture launches a POSIX shell, which this suite has no
/// native Windows equivalent for.
#[cfg(unix)]
async fn live_reusable_background_record(temp: &TempDir) -> (SubagentController, ExecSessionManager, String) {
    write_test_background_subagent(temp.path());
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let exec_sessions = controller.config.exec_sessions.clone();
    exec_sessions
        .create_pipe_session(
            LIVE_BACKGROUND_SESSION.to_string().into(),
            vec!["/bin/sh".to_string(), "-c".to_string(), "cat".to_string()],
            temp.path().to_path_buf(),
            hashbrown::HashMap::new(),
        )
        .await
        .expect("live background session");

    let spec = controller.resolve_requested_spec(Some("background-demo")).await.expect("spec");
    let record_id = background_record_id(spec.name.as_str());
    let created_at = Utc::now();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(
            record_id.clone(),
            BackgroundRecord {
                id: record_id.clone(),
                agent_name: spec.name.clone(),
                display_label: subagent_display_label(&spec),
                description: spec.description.clone(),
                source: spec.source.label(),
                colour: spec.colour.clone(),
                owner_session_id: None,
                session_id: "session-background-demo".to_string(),
                parent_session_id: "parent-session".to_string(),
                exec_session_id: LIVE_BACKGROUND_SESSION.to_string(),
                desired_enabled: true,
                status: BackgroundSubprocessStatus::Running,
                created_at,
                updated_at: created_at,
                started_at: Some(created_at),
                ended_at: None,
                pid: Some(42),
                prompt: "Report readiness once.".to_string(),
                summary: Some("ready".to_string()),
                error: None,
                close_fault: None,
                archive_path: None,
                transcript_path: None,
                max_turns: Some(4),
                model_override: None,
                reasoning_override: None,
                restart_attempts: 0,
            },
        );
    }

    (controller, exec_sessions, record_id)
}

#[tokio::test]
async fn spawn_background_subprocess_rejects_non_background_agent() {
    let temp = TempDir::new().expect("tempdir");
    let mut cfg = DiapasonConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new(test_controller_config(temp.path().to_path_buf(), cfg))
        .await
        .expect("controller");

    controller.set_turn_delegation_hints_from_input("delegate this task").await;

    let err = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("worker".to_string()),
            message: Some("Implement a change.".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect_err("non-background agent should be rejected");

    assert!(err.to_string().contains("background: true"));
    assert!(err.to_string().contains("Use spawn_agent instead"));
}

/// One active record guards both the reuse and the conflict path.
///
/// The two cases differ only in the requested prompt and the outcome, so they
/// share one fixture and one body: a change to the record contract cannot leave
/// one case building a fixture the other never exercises.
#[cfg(unix)]
#[rstest]
#[case::matching_settings(None, None)]
#[case::conflicting_settings(Some("Run a different task."), Some("different prompt"))]
#[tokio::test]
async fn spawn_background_subprocess_guards_an_active_record(
    #[case] message: Option<&str>,
    #[case] expected_error: Option<&str>,
) {
    let temp = TempDir::new().expect("tempdir");
    let (controller, exec_sessions, record_id) = live_reusable_background_record(&temp).await;

    let result = controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            message: message.map(str::to_string),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await;
    // Close before asserting so a failed assertion cannot leak the session.
    exec_sessions
        .close_session(LIVE_BACKGROUND_SESSION)
        .await
        .expect("close session");

    match expected_error {
        None => {
            let entry = result.expect("matching active record should be returned");
            assert_eq!(entry.id, record_id);
            assert_eq!(entry.status, BackgroundSubprocessStatus::Running);
            assert_eq!(
                entry.exec_session_id, LIVE_BACKGROUND_SESSION,
                "the reused record must name the live launch, not a replacement this call started"
            );
        }
        Some(expected) => {
            let err = result.expect_err("conflicting active record should be rejected");
            assert!(err.to_string().contains(expected));
            assert!(err.to_string().contains("Stop or restart"));
        }
    }
}
