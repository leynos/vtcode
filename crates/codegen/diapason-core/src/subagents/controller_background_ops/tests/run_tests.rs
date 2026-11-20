//! Exercises launch settlement, launch identity, and archive attachment.

use crate::config::DiapasonConfig;
use crate::subagents::{BackgroundSubprocessStatus, SubagentController, TaskOwner};
use tempfile::TempDir;

use super::super::super::BackgroundLaunchOverrides;
use super::super::BackgroundRunRequest;
use super::super::controller_background_archive::attach_background_archive_paths;
use super::super::controller_background_run::{background_exec_session_id, background_launch_session_id};
#[cfg(unix)]
use super::super::controller_background_run::{gate_next_background_commit, remove_spec_before_next_background_retry};
#[cfg(unix)]
use super::support::RecordingObserver;
#[cfg(unix)]
use super::support::session_is_gone;
#[cfg(unix)]
use super::support::write_background_demo_agent;
#[cfg(unix)]
use super::support::write_sleeping_demo_script;
use super::support::{background_record, background_test_controller, test_controller_config};
use super::{FakeOutput, FakeOutputReader};

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
    let entry = launch.await.expect("the declined launch reports the record it left");

    assert!(!entry.desired_enabled, "the cancelled record stays cancelled");
    assert_eq!(
        entry.status,
        BackgroundSubprocessStatus::Starting,
        "the declined commit must not write over the cancellation"
    );
    assert!(
        session_is_gone(&controller, &spawned_exec_session_id).await,
        "a declined commit must close the session it spawned; no record names it"
    );

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

    assert!(!committed, "a replaced launch must not settle the replacement");
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

    assert!(committed, "the launch owns this record and must settle it");
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

    assert!(!committed, "a launch must not adopt an exec session another record already holds");
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.status, BackgroundSubprocessStatus::Starting);
    assert_eq!(record.pid, None);

    drop(state);

    controller.signal_shutdown().await;
}

/// Two launches of one record in one process never reuse an exec-session id.
#[test]
fn every_generated_exec_session_id_is_unique() {
    let first = background_exec_session_id("session-background-worker");
    let second = background_exec_session_id("session-background-worker");

    assert_ne!(first, second, "a repeated id would make one launch fail on the duplicate-session check");
    assert!(first.starts_with("exec-session-background-worker-"), "{first}");
}

/// Two launches in one millisecond never share a session id.
///
/// The timestamp component of the session id only has millisecond resolution,
/// and the archive attachment uses the session id to identify a launch. A
/// shared id would let the earlier run's archive attach to its replacement, so
/// the identity must carry a process-unique sequence as well.
#[test]
fn every_generated_launch_session_id_is_unique() {
    let base = "parent-session-background-worker-20260101T000000000Z";
    let first = background_launch_session_id(base);
    let second = background_launch_session_id(base);

    assert_ne!(first, second, "two launches in one millisecond must not share a session id");
    assert!(first.starts_with(base), "{first}");
}

/// Archive paths attach only to the launch that named the resolved session.
#[tokio::test]
async fn archive_paths_attach_only_to_the_launch_that_named_the_session() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let record = background_record(
        "background-worker",
        None,
        "worker",
        "exec-launch",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }
    let archive = temp.path().join("archive.json");

    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-replaced", "exec-launch", &archive);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert!(
            record.archive_path.is_none(),
            "an archive resolved for a replaced launch must not attach to the replacement"
        );
        assert!(record.transcript_path.is_none());
    }

    let (session_id, exec_session_id) = {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        (record.session_id.clone(), record.exec_session_id.clone())
    };
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", &session_id, &exec_session_id, &archive);
    }
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.archive_path, Some(archive.clone()));
    assert_eq!(record.transcript_path, Some(archive.clone()));
    drop(state);

    controller.signal_shutdown().await;
}

/// A replacement launch with the same session id never inherits the archive.
///
/// The session id is derived from a millisecond timestamp, so a launch that
/// replaces another in the same millisecond can repeat it. The exec session
/// identifies the launch, so attachment requires both to match: otherwise the
/// earlier run's archive is written over the replacement, which then reports
/// output it never produced.
#[tokio::test]
async fn a_repeated_session_id_does_not_attach_the_earlier_launchs_archive() {
    let temp = TempDir::new().expect("tempdir");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;

    let mut first =
        background_record("background-worker", None, "worker", "exec-first", BackgroundSubprocessStatus::Running, true);
    first.session_id = "session-shared".to_string();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(first.id.clone(), first);
    }

    // A second launch of the same record started in the same millisecond, so
    // it derived the same session id but names its own exec session.
    let mut replacement = background_record(
        "background-worker",
        None,
        "worker",
        "exec-second",
        BackgroundSubprocessStatus::Starting,
        true,
    );
    replacement.session_id = "session-shared".to_string();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(replacement.id.clone(), replacement);
    }

    let archive = temp.path().join("earlier-run.json");
    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-shared", "exec-first", &archive);
    }

    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(record.exec_session_id, "exec-second", "the replacement owns the record");
        assert!(record.archive_path.is_none(), "the earlier launch's archive must not attach to its replacement");
        assert!(record.transcript_path.is_none());
    }

    {
        let mut state = controller.state.write().await;
        attach_background_archive_paths(&mut state, "background-worker", "session-shared", "exec-second", &archive);
    }
    {
        let state = controller.state.read().await;
        let record = state.background_children.get("background-worker").expect("record");
        assert_eq!(
            record.archive_path,
            Some(archive.clone()),
            "the launch that named the exec session records its own archive"
        );
    }

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
        .background_status_entries()
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

/// A launch never reaches another owner's record through its legacy alias.
///
/// The client-supplied id names a record in this controller's scope, but the
/// query fallback also matches `agent@session` ids. Applying it during a launch
/// let an unscoped controller resolve another owner's record and rewrite it
/// under its own view, so the launch lookup is exact and the request claims an
/// id of its own.
#[cfg(unix)]
#[tokio::test]
async fn a_launch_does_not_adopt_another_owners_record_through_a_legacy_alias() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    std::fs::remove_file(temp.path().join("scripts").join("demo-background-subagent.sh"))
        .expect("the launch script is unavailable");
    let controller = background_test_controller(temp.path(), DiapasonConfig::default()).await;
    let owned = background_record(
        "background-demo@session-owner",
        Some("session-owner"),
        "background-demo",
        "exec-foreign",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(owned.id.clone(), owned);
    }

    // The fixture cannot spawn; what matters is which record the launch named.
    let _error = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: Some("background-demo".to_string()),
            restart_attempts: 0,
            overrides: None,
            expected_exec_session_id: None,
        })
        .await;

    let state = controller.state.read().await;
    let owned = state.background_children.get("background-demo@session-owner").expect("record");
    assert_eq!(owned.exec_session_id, "exec-foreign", "another owner's launch keeps its process");
    assert_eq!(
        owned.status,
        BackgroundSubprocessStatus::Running,
        "the owner's record is not settled by this launch"
    );
    assert!(owned.desired_enabled, "another owner's launch stays wanted");
    assert!(
        state.background_children.contains_key("background-demo"),
        "the launch claims its own record instead of another owner's alias"
    );
    drop(state);

    controller.signal_shutdown().await;
}

/// An unscoped controller holding several owners' records resolves a session's
/// own record through a legacy alias, never an ownerless row.
///
/// The map is keyed by canonical owner-scoped id, while a client can hold the
/// pre-owner-scoping `background-<agent>` id. That ownerless key sorts before
/// `background-<agent>@<owner>`, so an owner-blind fallback selected the
/// ownerless row first and then rejected it as foreign, hiding the session's
/// own record behind a false "unknown". Resolution now narrows each candidate
/// by the caller's authority.
#[tokio::test]
async fn a_session_resolves_its_own_record_past_an_ownerless_legacy_row() {
    let temp = TempDir::new().expect("tempdir");
    let mut config = test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default());
    config.output_reader = Some(std::sync::Arc::new(FakeOutputReader::for_task(
        "background-worker@session-owner",
        FakeOutput::Live("owned output"),
    )));
    let controller = SubagentController::new(config).await.expect("controller");
    let legacy = background_record(
        "background-worker",
        None,
        "worker",
        "exec-legacy",
        BackgroundSubprocessStatus::Running,
        true,
    );
    let owned = background_record(
        "background-worker@session-owner",
        Some("session-owner"),
        "worker",
        "exec-owned",
        BackgroundSubprocessStatus::Running,
        true,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(legacy.id.clone(), legacy);
        state.background_children.insert(owned.id.clone(), owned);
    }

    let snapshot = controller
        .background_snapshot(TaskOwner::Session("session-owner"), "background-worker")
        .await
        .expect("the session resolves its own record through the legacy alias");
    assert_eq!(
        snapshot.entry.id, "background-worker@session-owner",
        "an ownerless legacy row must not shadow the session's own record"
    );

    let tail = controller
        .background_output_tail(TaskOwner::Session("session-owner"), "background-worker", 10)
        .await
        .expect("the session reads its own record's output through the legacy alias");
    assert_eq!(tail, "owned output", "the tail is read from the session's own record");

    let cancelled = controller
        .force_cancel_background(TaskOwner::Session("session-owner"), "background-worker")
        .await
        .expect("the session cancels its own record through the legacy alias");
    assert_eq!(cancelled.id, "background-worker@session-owner", "the stop names the session's own record");

    let state = controller.state.read().await;
    let legacy = state.background_children.get("background-worker").expect("legacy record");
    assert_eq!(legacy.status, BackgroundSubprocessStatus::Running, "the ownerless legacy row is untouched");
    assert!(legacy.desired_enabled, "the ownerless legacy row still wants its process");
    let owned = state
        .background_children
        .get("background-worker@session-owner")
        .expect("owned record");
    assert!(!owned.desired_enabled, "the session's own record is the one that was cancelled");
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

/// A parent override re-binds a terminal owner-scoped record without stealing
/// its owner.
///
/// An owner-scoped record's id encodes its owner, so it belongs to that session
/// for life; a launch may still name the session that now drives it. The
/// reservation must keep the stored owner and take the requested parent, and
/// the published snapshot must route by the new parent, not the old one.
#[cfg(unix)]
#[tokio::test]
async fn a_parent_override_rebinds_a_terminal_record_without_stealing_its_owner() {
    let temp = TempDir::new().expect("tempdir");
    write_background_demo_agent(temp.path());
    write_sleeping_demo_script(temp.path());
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.enabled = true;
    let controller = background_test_controller(temp.path(), vt_cfg).await;
    let record = background_record(
        "background-worker",
        Some("session-a"),
        "background-demo",
        "exec-previous",
        BackgroundSubprocessStatus::Stopped,
        false,
    );
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record.id.clone(), record);
    }

    let observer = std::sync::Arc::new(RecordingObserver::default());
    let registration = controller.register_progress_observer("other-session", observer.clone());

    let entry = controller
        .ensure_background_record_running(BackgroundRunRequest {
            agent_name: "background-demo".to_string(),
            stable_id: Some("background-worker".to_string()),
            restart_attempts: 0,
            overrides: Some(BackgroundLaunchOverrides {
                prompt: None,
                max_turns: None,
                model_override: None,
                reasoning_override: None,
                parent_session_id: Some("other-session".to_string()),
            }),
            expected_exec_session_id: None,
        })
        .await
        .expect("a terminal record can be re-launched under the parent that now drives it");

    assert_eq!(
        entry.owner_session_id.as_deref(),
        Some("session-a"),
        "an owner-scoped record keeps the owner its id encodes"
    );
    let state = controller.state.read().await;
    let record = state.background_children.get("background-worker").expect("record");
    assert_eq!(record.owner_session_id.as_deref(), Some("session-a"), "the reserved record keeps its owner");
    assert_eq!(
        record.parent_session_id, "other-session",
        "the override names the session that now drives the record"
    );
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Running,
        "the overridden parent's launch is committed as the record's run"
    );
    drop(state);

    let published = observer.background_entries();
    assert!(
        published
            .iter()
            .any(|entry| entry.id == "background-worker" && entry.status == BackgroundSubprocessStatus::Running),
        "the snapshot routes by parent_session_id, so the overridden parent's observer receives it"
    );

    drop(registration);
    let _ = controller.config.exec_sessions.close_session(&entry.exec_session_id).await;
    controller.signal_shutdown().await;
}
