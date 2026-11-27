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

    // Both spawn attempts fail with the same scripted error, so counting them
    // is what proves the inline retry ran: a settlement that skipped the retry
    // would leave the same terminal fields behind with a single attempt.
    assert_eq!(
        background_spawn_attempt_count(&controller.state),
        2,
        "the failed launch must spend its allowance on a second spawn attempt"
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
    {
        let state = controller.state.read().await;
        let record = state
            .background_children
            .get("background-background-demo")
            .expect("the failed launch leaves a record");
        assert_eq!(record.restart_attempts, 1, "the terminal record retains the allowance the retry spent");
    }

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

/// A launch dropped after its spawn is left to reconciliation.
///
/// The spawn registers its exec session before the commit, so a caller dropped
/// between them owns a live process. Settling it as a failed start would report
/// a running process as gone, and leaving the claim's non-retryable obligation
/// in place would exempt the record from every later observation, including an
/// observed exit. The real launch is parked at the production commit gate and
/// dropped there, so this case fails if the guard stops being armed on that
/// path. Refresh must then observe the same session, promote the durable claim
/// to `Running`, and write that promotion to the ledger.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_dropped_after_its_spawn_is_settled_by_refresh() {
    use super::super::super::controller_background_lifecycle::background_launch_is_owed;
    use super::super::support::{session_is_gone, write_background_demo_agent, write_sleeping_demo_script};
    use crate::subagents::background::load_background_state;
    use crate::subagents::background_record_id_for_owner;

    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    let (entered, _release) = gate_next_background_commit(&controller.state);
    let exec_session_id = {
        let launch = controller.ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: Some("background-worker".to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        });
        tokio::pin!(launch);
        tokio::select! {
            result = &mut launch => panic!("the gated launch settled before it was dropped: {result:?}"),
            () = entered.notified() => {}
        }
        // Dropping the pinned future here cancels the real launch inside the
        // production spawned-launch guard, which must hand the registered
        // session to reconciliation instead of settling it as a failed start.
        let state = controller.state.read().await;
        let record = state
            .background_children
            .get("background-worker")
            .expect("the claim is written");
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Starting,
            "the claim is still starting while its commit is pending"
        );
        record.exec_session_id.clone()
    };
    assert!(
        !background_launch_is_owed(&controller.state, "background-worker", &exec_session_id),
        "a dropped launch must not keep exempting its record from observation"
    );
    assert!(
        !session_is_gone(&controller, &exec_session_id).await,
        "the spawn registered a live session before the launch was dropped"
    );

    controller
        .refresh_background_processes()
        .await
        .expect("refresh settles the session the dropped launch registered");
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.exec_session_id, exec_session_id,
            "the record still names the session the dropped launch registered"
        );
        assert_eq!(
            record.status,
            BackgroundSubprocessStatus::Running,
            "refresh observes the same session and settles it as running, not as a failed start"
        );
    }
    // The promotion exists only in memory unless refresh owes and retries the
    // write, so the ledger is the decisive evidence that it was persisted. The
    // ledger keys a record by its canonical id, derived from the agent and
    // owner, not by the in-memory alias the request addressed this launch by.
    let canonical_id = background_record_id_for_owner("background-demo", None);
    let persisted = load_background_state(temp.path()).await.expect("reload the ledger");
    let stored = persisted
        .records
        .iter()
        .find(|row| row.id == canonical_id)
        .expect("the dropped launch's row reaches the ledger");
    assert_eq!(stored.exec_session_id, exec_session_id, "the durable row names the same launch refresh adopted");
    assert_eq!(
        stored.status,
        BackgroundSubprocessStatus::Running,
        "a refresh that adopts a spawned launch must persist the promotion it reports"
    );

    let _ = controller.config.exec_sessions.close_session(&exec_session_id).await;
    controller.signal_shutdown().await;
}

/// Reuse must not answer from memory when a commit owes the ledger a write.
///
/// The first owed-write check runs before the reservation takes the ledger lock.
/// A launch can commit `Running` in between, and its save needs that same lock,
/// so the ledger row still reads `Starting`. This parks the reservation at the
/// production gate between the two checks, commits the launch, and proves the
/// reservation waits for the row to become durable before it answers.
#[cfg(unix)]
#[tokio::test]
async fn a_reuse_that_races_a_commit_waits_for_the_ledger_row() {
    use super::super::super::controller_background_settlement::gate_next_background_claim_lock;
    use crate::subagents::background::background_state_path_for_tests;
    use crate::subagents::background_record_id_for_owner;

    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;

    // Launch 1 is parked before its commit, so its record is an unfinished
    // claim in memory while its process is already registered.
    let (commit_entered, commit_release) = gate_next_background_commit(&controller.state);
    let launching = controller.clone();
    let mut launch = tokio::spawn(async move {
        launching
            .ensure_background_record_running(BackgroundRunRequest {
                agent_name: "background-demo".to_string(),
                stable_id: Some("background-worker".to_string()),
                restart_attempts: 0,
                overrides: None,
                expected_exec_session_id: None,
            })
            .await
    });
    tokio::select! {
        result = &mut launch => panic!("the launch settled before its commit was parked: {result:?}"),
        () = commit_entered.notified() => {}
    }
    let claimed_exec_session_id = {
        let state = controller.state.read().await;
        state
            .background_children
            .get("background-worker")
            .expect("the claim is written before the commit")
            .exec_session_id
            .clone()
    };

    // Reservation 2 is parked after taking the ledger lock and before the
    // repeated owed-write check.
    let (lock_entered, lock_release) = gate_next_background_claim_lock(&controller.state);
    let reusing = controller.clone();
    let mut reuse = tokio::spawn(async move {
        reusing
            .ensure_background_record_running(BackgroundRunRequest {
                agent_name: "background-demo".to_string(),
                stable_id: Some("background-worker".to_string()),
                restart_attempts: 0,
                overrides: None,
                expected_exec_session_id: None,
            })
            .await
    });
    tokio::select! {
        result = &mut reuse => panic!("reuse answered before it held the ledger lock: {result:?}"),
        () = lock_entered.notified() => {}
    }

    // Launch 1 commits `Running`; its save is blocked on the ledger lock that
    // reservation 2 holds, so the row still reads the unfinished claim.
    commit_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if pending_background_ledger_write_generation(&controller.state).is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the commit raises its owed write before reuse answers");

    // The claim is durable under the record's canonical id, which the ledger
    // derives from the agent and owner; the request addressed the record by
    // the `background-worker` alias, so that id never appears in the file.
    let canonical_id = background_record_id_for_owner("background-demo", None);
    let ledger_path = background_state_path_for_tests(temp.path());
    let stored = std::fs::read_to_string(&ledger_path).expect("the claim is durable");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let row = ledger["records"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == canonical_id.as_str()))
        .expect("the claim row is in the ledger");
    assert_eq!(row["status"], "starting", "the committed record is not durable while reuse holds the ledger lock");
    assert!(!reuse.is_finished(), "reuse must not answer from memory while the write is owed");

    lock_release.notify_one();
    let entry = reuse
        .await
        .expect("the reuse task joins")
        .expect("reuse answers once the record is durable");
    assert_eq!(entry.exec_session_id, claimed_exec_session_id, "reuse answers for the launch that was committed");
    let stored = std::fs::read_to_string(&ledger_path).expect("the owed write reaches the ledger");
    let ledger: serde_json::Value = serde_json::from_str(&stored).expect("the ledger is valid JSON");
    let row = ledger["records"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["id"] == canonical_id.as_str()))
        .expect("the committed row is in the ledger");
    assert_eq!(row["status"], "running", "reuse may only answer once the row names the running launch");

    let _ = launch.await.expect("the launch task joins");
    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}
