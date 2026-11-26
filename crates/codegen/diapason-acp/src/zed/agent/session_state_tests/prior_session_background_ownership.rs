//! A new ACP session must not adopt another session's background work.
use super::archive::archived_listing;
use super::background_restore::{
    ClearLedgerLockOnBuild, launch_count, restored_background_config, seed_launched_background_ledger,
    write_background_restore_fixture_with_marker, write_launch_counting_fixture,
};
use super::*;
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use diapason_core::subagents::{BackgroundSubprocessStatus, TaskOwner, background_record_id_for_owner};

/// A new session may not restore a background record another session owns.
///
/// The seeded row names a different owner and a runnable worker whose script
/// writes a marker, so a restore attempt would really execute it. Neither the
/// fresh session's controller nor the workspace scope may claim the row: only
/// the owning session can restore it, and a new session cannot become that
/// owner. Both boundaries are asserted, because the two scopes resolve
/// ownership independently: a session controller answers for its own records,
/// and the workspace controller answers for the ownerless ones a resume may
/// leave behind. The ledger is read back after setup so the check covers the
/// persisted launch identity a restore would have replaced, not only the copy
/// the launch controller happens to hold.
#[tokio::test]
async fn new_session_does_not_restore_a_background_record_owned_by_another_session() {
    let temp = TempDir::new().unwrap();
    let workspace = temp.path();
    let state_dir = workspace.join(".diapason/state");
    fs::create_dir_all(&state_dir).unwrap();
    let marker = workspace.join("background-restored-marker");
    // The demo worker resolves to the workspace script, so a restore by either
    // scope would run it and write the marker.
    write_background_restore_fixture_with_marker(workspace, Some(&marker));
    let ledger_path = state_dir.join("background_subagents.json");
    fs::write(
        &ledger_path,
        serde_json::json!({
            "records": [{
                "id": "foreign-worker",
                "agent_name": "background-demo",
                "display_label": "background-demo",
                "description": "background-demo",
                "source": "builtin",
                "color": null,
                "owner_session_id": "prior-session",
                "session_id": "child-session",
                "exec_session_id": "exec-session",
                "desired_enabled": true,
                "status": "stopped",
                "created_at": "2026-08-28T00:00:00Z",
                "updated_at": "2026-08-28T00:00:00Z",
                "started_at": null,
                "ended_at": null,
                "pid": null,
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
        .to_string(),
    )
    .unwrap();

    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(workspace, "duck", Some(&vt_config)).await;
    // The launch runtime is built from the same workspace before any session
    // exists, so its controller is the scope a fresh attach would fall back to.
    let launch_controller = agent
        .local_tool_registry
        .subagent_controller()
        .expect("the launch workspace controller");
    let response = agent.new_session(acp::NewSessionRequest::new(workspace)).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let controller = agent
        .session_subagent_controller(&session)
        .unwrap()
        .expect("per-session subagent controller");

    assert!(
        controller
            .background_status_entries(TaskOwner::Session(response.session_id.0.as_ref()))
            .await
            .is_empty(),
        "the new session must not adopt a record another session owns"
    );
    assert!(
        controller.background_status_entries(TaskOwner::Workspace).await.is_empty(),
        "the new session's controller must hold no copy of another session's record"
    );
    // The workspace scope must not restore the record either: only the owning
    // session may bring it back.
    assert!(
        launch_controller
            .restore_legacy_background_subagents()
            .await
            .expect("the workspace restore pass must not fail on another session's record")
            .is_empty(),
        "the workspace scope must not restore a record a prior session owns"
    );
    let workspace_view = launch_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(workspace_view.len(), 1, "the launch controller loads the seeded ledger");
    let foreign = &workspace_view[0];
    assert_eq!(
        foreign.id,
        background_record_id_for_owner("background-demo", Some("prior-session")),
        "the loaded record keeps the owner-scoped id of the session it belongs to"
    );
    assert_eq!(
        foreign.owner_session_id.as_deref(),
        Some("prior-session"),
        "the workspace scope must leave the owning session's record with its owner"
    );

    // The persisted ledger is the launch identity a restore would have
    // replaced, so it must still name the seeded exec session.
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after setup");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after setup");
    let record = &persisted["records"][0];
    assert_eq!(
        record["exec_session_id"], "exec-session",
        "neither scope may replace the owning session's launch identity: {persisted}"
    );
    assert_eq!(
        record["owner_session_id"].as_str(),
        Some("prior-session"),
        "the persisted record keeps its owner: {persisted}"
    );
    assert_eq!(record["status"], "stopped", "the persisted record was never relaunched: {persisted}");
    assert!(record["pid"].is_null(), "a restored record would have persisted a pid: {persisted}");
    assert_eq!(
        foreign.status,
        BackgroundSubprocessStatus::Stopped,
        "the loaded record keeps the seeded stopped status"
    );

    // The fixture script exits at once, so a spawn by either scope would write
    // the marker almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!marker.exists(), "a runnable worker owned by another session must never execute");
}

/// A migration retry that succeeds must be adopted before the workspace
/// restore is released.
///
/// The resume-site migration fails on an obstructed ledger lock, so the
/// workspace controller loads the row ownerless. The session controller's retry
/// then succeeds and writes the owner. The fixture script appends one line per
/// launch, so the count shows whether a workspace restore duplicated the row.
#[tokio::test]
async fn a_migration_retry_that_succeeds_is_adopted_before_the_workspace_restore() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the retried-adoption test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let launches = session_workspace.join("background-launch-count");
    write_launch_counting_fixture(&session_workspace, &launches);
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let session_id = acp::SessionId::new("background-retried-adoption-session");
    let ledger_path = seed_launched_background_ledger(&state_dir, session_id.0.as_ref());
    let lock_path = state_dir.join("background_subagents.lock");
    fs::create_dir(&lock_path).expect("occupy the ledger lock path with a directory");
    let vt_config = restored_background_config();
    let mut agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    agent.workspace_setup_ports = agent
        .workspace_setup_ports
        .clone()
        .with_file_ops_tool_construction(Arc::new(ClearLedgerLockOnBuild { lock_path: lock_path.clone() }));
    let listing = archived_listing("background-retried-adoption", &session_workspace, "continue");

    let _handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
        .expect("attach must continue after the migration retry succeeds");

    // The retry wrote the owner, so the session's own restore launched the row.
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the attach");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the attach");
    let owned = persisted["records"]
        .as_array()
        .expect("ledger records")
        .iter()
        .find(|record| record["owner_session_id"].as_str() == Some(session_id.0.as_ref()))
        .expect("the retry must have given the row to the resumed session");
    assert_ne!(
        owned["exec_session_id"], "exec-missing",
        "the resumed session must restore the row it now owns: {persisted}"
    );

    // A duplicate workspace launch would record its own line shortly after the
    // attach returns, so poll briefly before asserting the count.
    for _ in 0..40 {
        if launch_count(&launches) >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert_eq!(
        launch_count(&launches),
        1,
        "only the session scope may launch the migrated row; a workspace launch would duplicate it"
    );
}

/// One session's reconciled ownership must not release another session's
/// deferred restore.
///
/// Session A's migration and retry leave its row ownerless, so A records itself
/// as a blocker. Session B then resumes in the same workspace and reconciles its
/// own ownership, which says nothing about A's row. If B's release made the
/// shared restore payable, the workspace scope would relaunch A's row as
/// workspace work; the marker proves whether that happened.
#[tokio::test]
async fn a_second_resume_cannot_release_a_restore_another_sessions_ownership_blocks() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the shared debt test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let session_a = acp::SessionId::new("background-blocked-owner-session");
    let ledger_path = seed_launched_background_ledger(&state_dir, session_a.0.as_ref());
    let lock_path = state_dir.join("background_subagents.lock");
    fs::create_dir(&lock_path).expect("occupy the ledger lock path with a directory");
    let vt_config = restored_background_config();
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let listing_a = archived_listing("background-blocked-owner", &session_workspace, "continue");
    let handle_a = agent
        .attach_thread_from_listing(
            &session_a,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing_a,
        )
        .await
        .expect("attach must continue when background owner migration fails");
    let runtime_a = {
        let data = handle_a.data.lock().expect("lock the attached session data");
        data.workspace_runtime
            .clone()
            .expect("attach must retain its workspace runtime")
    };
    assert!(
        runtime_a.pending_background_restore_is_blocked(),
        "the unresolved session must hold the workspace restore back"
    );
    assert!(
        !runtime_a.pending_background_restore_is_payable(),
        "a restore blocked by unresolved ownership must not be payable"
    );

    // With the obstruction cleared, the second session can migrate and
    // reconcile its own rows, but nothing it does can reconcile the first
    // session's row.
    fs::remove_dir(&lock_path).expect("clear the ledger lock obstruction");
    let session_b = acp::SessionId::new("background-reconciled-session");
    let listing_b = archived_listing("background-reconciled", &session_workspace, "continue");
    let handle_b = agent
        .attach_thread_from_listing(
            &session_b,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing_b,
        )
        .await
        .expect("the second resume must attach");
    let runtime_b = {
        let data = handle_b.data.lock().expect("lock the second attached session data");
        data.workspace_runtime
            .clone()
            .expect("the second attach must retain its workspace runtime")
    };
    assert!(Arc::ptr_eq(&runtime_a, &runtime_b), "both sessions must share the cached workspace runtime");
    assert!(
        runtime_b.pending_background_restore_is_blocked(),
        "a reconciled session must not clear another session's blocker"
    );
    assert!(
        !runtime_b.pending_background_restore_is_payable(),
        "the restore must stay unpayable while the first session's row is ownerless"
    );

    // The fixture script exits at once, so a workspace launch would write the
    // marker almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!marker.exists(), "the workspace scope must not restore a row another session's ownership blocks");
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the second resume");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the second resume");
    assert_eq!(
        persisted["records"][0]["exec_session_id"], "exec-missing",
        "no scope may launch the blocked row: {persisted}"
    );
    drop((handle_a, handle_b));
}
