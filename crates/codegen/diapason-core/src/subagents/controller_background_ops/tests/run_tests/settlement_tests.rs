//! Launch settlement, retry settlement and conflicting launches.
//!
//! Declared by `run_tests/mod.rs`. Pins what a launch commit does to a record it
//! owns or no longer owns, when a failed launch settles `Error`, and when a
//! stale or conflicting launch must not replace the current record.
use super::*;

/// A launch that a cancellation overtook must not revive the record, and the
/// session it spawned must be closed.
///
/// The commit re-checks the record after the spawn runs without the state lock.
/// A cancellation landing in that window clears `desired_enabled`, so the
/// commit declines and leaves the record as the cancellation left it. The
/// spawned process is then named by no record, and only the declining launch
/// can close it: the cancellation never saw the session. The gate lands the
/// cancellation deterministically instead of racing a real process.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_does_not_revive_a_record_that_stopped_wanting_it() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    let record = background_record(
        "background-worker",
        None,
        "background-demo",
        "exec-previous",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let (entered, release) = gate_next_background_commit(&controller.state);
    let launch = controller.ensure_background_record_running(BackgroundRunRequest {
        agent_name: "background-demo".to_string(),
        stable_id: Some("background-worker".to_string()),
        restart_attempts: 0,
        overrides: None,
        expected_exec_session_id: None,
    });
    tokio::pin!(launch);
    let spawned_exec_session_id = tokio::select! {
        result = &mut launch => panic!("the gated commit settled before the cancellation landed: {result:?}"),
        () = entered.notified() => {
            let mut state = controller.state.write().await;
            let record = state.background_children.get("background-worker").expect("record");
            assert_eq!(
                record.status,
                BackgroundSubprocessStatus::Starting,
                "the record is reserved while its spawn is in flight"
            );
            let exec_session_id = record.exec_session_id.clone();
            // A cancellation landing in the window clears the intent before it
            // closes anything. Clearing it here leaves the declined commit as
            // the only closer, so the liveness check below proves that path.
            let record = state.background_children.get_mut("background-worker").expect("record");
            record.desired_enabled = false;
            exec_session_id
        }
    };
    assert!(
        !session_is_gone(&controller, &spawned_exec_session_id).await,
        "the launch spawned a live session before the commit declined it"
    );

    release.notify_one();
    let error = launch
        .await
        .expect_err("a launch that lost its record is rejected rather than answered for another one");
    assert!(
        format!("{error:#}").contains("was stopped or replaced before its launch could be recorded"),
        "the caller is told its launch never reached a record: {error:#}"
    );
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert!(!record.desired_enabled, "the cancelled record stays cancelled");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "the declined commit must not write over the cancellation"
        );
    }
    assert!(
        session_is_gone(&controller, &spawned_exec_session_id).await,
        "a declined commit must close the session it spawned; no record names it"
    );

    controller.signal_shutdown().await;
}

/// A launch with no parent override adopts the controller's current parent.
///
/// The planner treats an empty stored parent as absent and falls back to the
/// controller's current parent. Calling the fallback helper directly and
/// applying `unwrap_or` in the test proved only that `Option::unwrap_or`
/// works; this drives the production chain through a real launch and reads the
/// parent the committed record carries.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_without_a_parent_override_uses_the_controllers_current_parent() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    controller.set_parent_session_id("current-parent").await;
    let mut record = background_record(
        "background-worker",
        None,
        "background-demo",
        "exec-previous",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    record.parent_session_id = String::new();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let entry = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: Some("background-worker".to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect("the launch starts with the controller's current parent");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(
        record.parent_session_id, "current-parent",
        "an empty stored parent must yield to the controller's current parent, not its configured one"
    );
    assert_eq!(
        record.exec_session_id, entry.exec_session_id,
        "the committed record names the launch the plan started"
    );
    drop(state);

    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}

/// A launch answers only for the exec session it created.
#[tokio::test]
async fn a_launch_does_not_settle_a_record_that_names_another_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-replacement",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-launch", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(committed.is_none(), "a replaced launch must not settle the replacement");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.exec_session_id, "exec-replacement");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);

    drop(state);

    controller.signal_shutdown().await;
}

/// A desired record is committed with the launch's process metadata.
#[tokio::test]
async fn a_launch_commits_to_its_own_desired_record() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let mut record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    record.ended_at = Some(chrono::Utc::now());
    record.error = Some("previous failure".to_string());
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-launch", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    let write_generation = committed.expect("the launch owns this record and must settle it");
    // The owed-write obligation is raised under the same lock that makes the
    // record reusable, so a concurrent request cannot treat it as durable.
    assert!(
        pending_background_ledger_write_generation(&controller.state).is_some(),
        "the committed record must owe its ledger write before it becomes reusable"
    );
    clear_background_ledger_write_pending(&controller.state, write_generation);
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Running);
    assert_eq!(record.pid, Some(7));
    assert_eq!(record.ended_at, None, "an active record must not carry the previous run's end instant");
    assert_eq!(record.error, None, "a running record must not carry the previous run's failure");

    drop(state);

    controller.signal_shutdown().await;
}

/// Two records may not share one exec-session id.
#[tokio::test]
async fn a_launch_cannot_share_an_exec_session_with_another_record() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let other =
        background_record("background-other", None, "other", "exec-shared", BackgroundSubprocessStatus::Running, true);
    let candidate = background_record(
        "background-worker",
        None,
        "worker",
        "exec-shared",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(other.id.clone(), other);
        state.background_children.insert(candidate.id.clone(), candidate);
    }

    let committed = controller
        .commit_background_run_started("background-worker", "exec-shared", Some(7), None)
        .await
        .expect("the commit answers whether it applied");

    assert!(committed.is_none(), "a launch must not adopt an exec session another record already holds");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.pid, None);

    drop(state);

    controller.signal_shutdown().await;
}

/// A launch that fails twice settles `Error` instead of staying `Starting`.
///
/// The first failure leaves the record `Starting` because the single restart
/// allowance remains, and nothing else drives that state. The launch itself
/// has to consume the allowance and retry; if the retry fails too, the record
/// must become a visible failure rather than an active process forever.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_launch_settles_after_the_retry_is_spent() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("the launch script is unavailable");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.auto_restore = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: None,
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect_err("a missing launch script fails the spawn");
    assert!(
        format!("{error:#}").contains("Failed to spawn background subprocess"),
        "the caller receives the spawn error: {error:#}"
    );

    let entry = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.agent_name == "background-demo")
        .expect("the failed launch leaves a record");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Error,
        "a spent restart allowance must settle the record as a visible failure"
    );
    assert!(entry.ended_at.is_some(), "a failed launch has ended");
    assert!(
        entry
            .error
            .as_deref()
            .is_some_and(|error| error.contains("Failed to start background subagent")),
        "{:?}",
        entry.error
    );

    controller.signal_shutdown().await;
}

/// A retry whose replacement cannot be prepared settles the record it replaces.
///
/// A spawn failure leaves the record `Starting` while the single restart
/// allowance remains, and the inline retry spends that allowance before it
/// prepares the replacement. The preparation itself can fail — the agent no
/// longer resolves — and nothing else drives a record whose replacement cannot
/// be prepared. The record must settle `Error` rather than report a launch that
/// will never run, and the spent allowance must stay spent so a later refresh
/// does not hand out a second replacement.
#[cfg(unix)]
#[tokio::test]
async fn a_retry_that_cannot_prepare_settles_the_record_it_replaces() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("the launch script is unavailable");
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    vt_cfg.subagents.background.auto_restore = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    // The first, unbound preparation still resolves the agent; the seam drops
    // the spec only for the bound retry that the spent allowance triggers.
    remove_spec_before_next_background_retry(&controller.state);

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: None,
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await
        .expect_err("the retry cannot prepare its replacement");

    assert!(
        format!("{error:#}").contains("Failed to resolve background subagent 'background-demo'"),
        "the caller receives the preparation error: {error:#}"
    );

    let state = controller.state.read().await;
    let record = state
        .background_children
        .get("background-background-demo")
        .expect("the failed launch leaves a record");
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Error,
        "a record whose replacement cannot be prepared must not stay Starting"
    );
    assert_eq!(record.restart_attempts, 1, "the inline retry spent the single restart allowance");
    assert!(record.ended_at.is_some(), "a settled preparation failure has ended");
    assert!(record.error.as_deref().is_some_and(|error| error.contains("background-demo")), "{:?}", record.error);
    drop(state);

    controller.signal_shutdown().await;
}

/// A restart bound to an observed launch must not replace a newer launch.
///
/// Refresh decides to restart from an observation it made outside the state
/// lock. Another controller can commit a newer launch before the restart runs,
/// and an unbound replacement would take the record id back, silently
/// displacing a process that is still running.
#[tokio::test]
async fn a_stale_restart_does_not_replace_a_newer_committed_launch() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record =
        background_record("background-worker", None, "explorer", "exec-new", BackgroundSubprocessStatus::Running, true);
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    controller
        .restart_after_background_observation("background-worker", "explorer".to_string(), 0, Some("exec-old"))
        .await
        .expect("a restart for a replaced launch is declined, not applied");

    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.exec_session_id, "exec-new", "the newer launch keeps the record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Running);
    assert_eq!(record.pid, None, "the declined restart must not attach a process");
    drop(state);

    controller.signal_shutdown().await;
}

/// An active launch with different settings is refused, not replaced.
///
/// The reuse check ran without the state lock before the record was written
/// unconditionally, so two launches could both pass and the second replaced a
/// running one. The reservation re-checks the active record under the write
/// lock and refuses a plan that asks for different work.
#[tokio::test]
async fn a_conflicting_launch_does_not_replace_an_active_run() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "explorer",
        "exec-live",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "explorer".to_string(),
            stable_id: Some("background-worker".to_string()),
            restart_attempts: 0,
            overrides: Some(BackgroundLaunchOverrides {
                prompt: Some("a different prompt".to_string()),
                max_turns: None,
                model_override: None,
                reasoning_override: None,
                parent_session_id: Some("parent-session".to_string()),
            }),
            expected_exec_session_id: None,
        })
        .await
        .expect_err("a conflicting launch must not replace the active record");

    assert!(
        format!("{error:#}").contains("active background subprocess 'explorer'"),
        "the caller is told which launch conflicts: {error:#}"
    );
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.exec_session_id, "exec-live", "the active launch keeps its process");
    assert_eq!(record.status, BackgroundSubprocessStatus::Running);
    assert_eq!(record.prompt, "work", "the refused plan must not rewrite the active launch");
    drop(state);

    controller.signal_shutdown().await;
}
