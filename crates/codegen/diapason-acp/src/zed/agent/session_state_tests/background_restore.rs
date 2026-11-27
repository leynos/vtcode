//! Background restore ordering across a failed ACP archive attach.
//!
//! A resumed session defers restoring its background sub-agents until its
//! repaired history is durably checkpointed. A restored child can outlive a
//! failed attach, because the fail-closed path releases the workspace lease but
//! shutdown aborts only tracked children, never restored background children.
//!
//! Three properties keep a failed attach from starting work it cannot stop,
//! from starting the same work twice, and from losing the work it owes:
//!
//! * a resumed attach must not restore a background sub-agent — its own or the
//!   workspace scope's — before its required history checkpoint succeeds;
//! * only a resume that reconciled its ownership may release the deferred
//!   workspace restore, because the ownerless rows it would relaunch may yet be
//!   adopted by the session controller on a retry; and
//! * a restore pass that fails keeps its obligation, so a later acquisition
//!   retries it instead of leaving the ownerless rows unrestored.
use super::archive::archived_listing;
use super::*;
use crate::zed::agent::optional_setup::OptionalSetupOutcome;
use crate::zed::agent::session_workspace::WorkspaceFileOpsToolConstruction;
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use diapason_core::config::AgentClientProtocolZedConfig;
use diapason_core::llm::provider::ToolCall;
use diapason_core::subagents::TaskOwner;
use diapason_core::tools::file_ops::FileOpsTool;
use diapason_core::tools::grep_file::GrepSearchManager;

/// A failed attach must not restore the background sub-agents it will own.
///
/// Restoring eagerly would launch a child before the required history
/// checkpoint. The fail-closed path releases the workspace lease, but shutdown
/// aborts only tracked children, never restored background children, so a
/// child launched before the failure would survive it. The persisted ledger is
/// the observable that proves no launch happened.
#[tokio::test]
async fn failed_attach_does_not_restore_background_subagents() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the background-restore test");
    write_background_restore_fixture(temp.path());
    let session_id = acp::SessionId::new("background-restore-checkpoint-failure-session");
    let ledger_path = temp
        .path()
        .join(diapason_commons::workspace_dir_name())
        .join("state")
        .join("background_subagents.json");
    fs::create_dir_all(ledger_path.parent().expect("background ledger parent"))
        .expect("create the background state directory");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "owner_session_id": session_id.0.as_ref(),
            "session_id": "session-background-demo-restore",
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    let seeded_bytes = serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger");
    fs::write(&ledger_path, &seeded_bytes).expect("seed the background ledger");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let mut listing = archived_listing("background-restore-failure", temp.path(), "continue");
    listing.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "restore-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    ))];
    listing.snapshot.total_messages = 1;
    fs::create_dir(&listing.path).expect("occupy the archive path with a directory");

    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(temp.path())
                .await
                .expect("build the ACP session workspace from the temporary directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a directory at the archive path must make the required checkpoint fail"),
        Err(error) => error,
    };

    assert_eq!(
        error.to_string(),
        "Failed to persist repaired ACP tool-call history: archive_write_failed",
        "the deadline must fail at the required checkpoint"
    );
    let persisted_bytes = fs::read(&ledger_path).expect("read the background ledger after the failed attach");
    assert_eq!(
        persisted_bytes, seeded_bytes,
        "a failed attach must not restore a background sub-agent before its checkpoint succeeds"
    );
    let persisted: serde_json::Value =
        serde_json::from_slice(&persisted_bytes).expect("parse the ledger the failed attach left on disk");
    let record = &persisted["records"][0];
    assert_eq!(record["status"], "stopped", "a failed attach must leave the record stopped: {persisted}");
    assert_eq!(
        record["exec_session_id"], "exec-missing",
        "a failed attach must not replace the record's launch identity: {persisted}"
    );
    assert_eq!(
        record["owner_session_id"].as_str(),
        Some(session_id.0.as_ref()),
        "a failed attach must leave the seeded ownership untouched: {persisted}"
    );
    assert!(record["pid"].is_null(), "a restored record must not keep a live pid: {persisted}");
    assert_eq!(
        record["desired_enabled"], true,
        "a failed attach must not clear the record's desire to run: {persisted}"
    );
    assert!(
        agent
            .session_handle(&session_id)
            .expect("query the session registry after the failed attach")
            .is_none(),
        "a failed attach must not register the session"
    );
}

/// A failed owner migration must stop the workspace controller restoring the
/// ownerless rows the session controller may yet adopt.
///
/// The migration is made to fail deterministically by occupying the ledger's
/// sidecar lock path with a directory: every lock attempt then fails
/// immediately, while the controller's ledger load still succeeds. The session
/// workspace differs from the launch workspace so this attach must build its
/// own workspace runtime rather than reuse the launch one. The fixture script
/// writes a marker, which proves whether the workspace restore ran even though
/// the save that would record the launch cannot take the lock.
#[tokio::test]
async fn failed_background_owner_migration_defers_workspace_restore() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the migration-failure test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = state_dir.join("background_subagents.json");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "session_id": "session-background-demo-migration",
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    fs::write(&ledger_path, serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger"))
        .expect("seed the background ledger");
    fs::create_dir(state_dir.join("background_subagents.lock")).expect("occupy the ledger lock path with a directory");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let session_id = acp::SessionId::new("background-migration-failure-session");
    let listing = archived_listing("background-migration-failure", &session_workspace, "continue");

    let handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
        .expect("attach must continue when background owner migration fails");

    // The ledger cannot record the launch because the save fails, so the
    // controller's in-memory record is the only durable evidence of whether a
    // restore ran. A launch always replaces the exec session the ledger names,
    // and rollback settles the record without restoring that id, so this
    // assertion holds whether or not the spawned script managed to write the
    // marker before it was cancelled.
    let workspace_runtime = {
        let data = handle.data.lock().expect("lock the attached session data");
        data.workspace_runtime
            .clone()
            .expect("attach must retain its workspace runtime")
    };
    // A build-time restore attempt would have failed on the obstructed ledger
    // lock and degraded the recorded outcome, so `Attached` is an invocation
    // observable that does not depend on the launch path the lock masks.
    assert_eq!(
        workspace_runtime.optional_setup_outcomes.subagent_controller,
        OptionalSetupOutcome::Attached,
        "the workspace controller must not attempt a restore this attach deferred"
    );
    assert!(
        workspace_runtime.pending_background_restore_is_blocked(),
        "the deferred restore must stay owed and blocked"
    );
    assert!(
        !workspace_runtime.pending_background_restore_is_payable(),
        "a deferred restore must not become payable before ownership is reconciled"
    );
    let controller = workspace_runtime
        .local_tool_registry
        .subagent_controller()
        .expect("the workspace runtime must retain its subagent controller");
    let entries = controller.background_status_entries(TaskOwner::Workspace).await;
    let record = entries
        .iter()
        .find(|entry| entry.id == "background-background-demo")
        .expect("the workspace controller must have loaded the seeded record");
    assert_eq!(
        record.exec_session_id, "exec-missing",
        "a deferred restore must leave the ownerless record unlaunched"
    );

    // The fixture script exits at once, so a spawned restore would write the
    // marker almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !marker.exists(),
        "a failed owner migration must stop the workspace controller restoring the ownerless row"
    );
}

/// A session that never migrates cannot release a restore a resume deferred.
///
/// The deferred debt exists because rows a resumed session launched may still be
/// ownerless. A `session/new` acquisition never migrates, so its success is no
/// evidence that ownership was reconciled; releasing the debt on it would let
/// the workspace scope relaunch work a live session may still adopt. The
/// persisted launch identity is the observable: a restore would have replaced
/// the exec session the ledger names.
#[tokio::test]
async fn a_session_new_acquisition_does_not_release_a_restore_an_unresolved_migration_blocks() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the deferred-restore test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = seed_restorable_background_ledger(&state_dir, "deferred");
    fs::create_dir(state_dir.join("background_subagents.lock")).expect("occupy the ledger lock path with a directory");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let session_id = acp::SessionId::new("background-deferred-restore-session");
    let listing = archived_listing("background-deferred-restore", &session_workspace, "continue");

    let handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
        .expect("attach must continue when background owner migration fails");
    let workspace_runtime = {
        let data = handle.data.lock().expect("lock the attached session data");
        data.workspace_runtime
            .clone()
            .expect("attach must retain its workspace runtime")
    };

    // The migration failed and the retry could not take the ledger lock either,
    // so rows this session launched are still ownerless. The deferred restore
    // must still be owed and blocked, not dropped.
    assert!(
        workspace_runtime.pending_background_restore_is_blocked(),
        "a restore deferred by a failed migration must stay owed instead of being dropped"
    );

    // A later acquisition for the same workspace reuses the cached runtime. It
    // never migrates, so it cannot prove ownership was reconciled and must leave
    // the debt blocked rather than releasing and running it.
    let workspace = AcpSessionWorkspace::from_client_cwd(&session_workspace)
        .await
        .expect("build the reusing ACP session workspace");
    let reuse = agent
        .workspace_runtime_for(&workspace, None)
        .await
        .expect("a later acquisition must reuse the cached runtime");
    assert!(
        Arc::ptr_eq(reuse.runtime(), &workspace_runtime),
        "the later acquisition must reuse the runtime the attach built"
    );
    assert!(
        workspace_runtime.pending_background_restore_is_blocked(),
        "an acquisition that never migrates must not release a debt an unresolved migration blocks"
    );
    assert!(
        !workspace_runtime.pending_background_restore_is_payable(),
        "the blocked debt must not become payable on a session/new acquisition"
    );
    drop(reuse);

    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the reuse");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the reuse");
    assert_eq!(
        persisted["records"][0]["exec_session_id"], "exec-missing",
        "no restore may run while ownership is unresolved: {persisted}"
    );

    // The fixture script exits at once, so a restore would write the marker
    // almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !marker.exists(),
        "the workspace scope must not restore a row while a resume's ownership is unresolved"
    );
}

/// A resumed attach defers the workspace restore until its required checkpoint.
///
/// The migration succeeds here, which says nothing about when the workspace
/// scope may restore ownerless rows: a child it starts before the attach's
/// required history checkpoint can outlive an attach that then fails, because
/// shutdown aborts only tracked children. The checkpoint is made to fail
/// deterministically by occupying the archive path, and the untouched persisted
/// launch identity proves the restore never ran.
#[tokio::test]
async fn a_resumed_attach_defers_the_workspace_restore_until_its_checkpoint() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the checkpoint-ordering test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = seed_restorable_background_ledger(&state_dir, "checkpoint");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let session_id = acp::SessionId::new("background-checkpoint-ordering-session");
    let mut listing = archived_listing("background-checkpoint-ordering", &session_workspace, "continue");
    listing.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "deferred-restore-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    ))];
    listing.snapshot.total_messages = 1;
    fs::create_dir(&listing.path).expect("occupy the archive path with a directory");

    let error = match agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
    {
        Ok(_) => panic!("a directory at the archive path must make the required checkpoint fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "Failed to persist repaired ACP tool-call history: archive_write_failed",
        "the deadline must fail at the required checkpoint"
    );

    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the failed attach");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the failed attach");
    let record = &persisted["records"][0];
    assert_eq!(
        record["exec_session_id"], "exec-missing",
        "a failed attach must not restore a child before its checkpoint: {persisted}"
    );
    assert_eq!(record["status"], "stopped", "the pending restore must leave the record stopped: {persisted}");
    assert!(record["pid"].is_null(), "a restored record would have persisted a pid: {persisted}");

    // The fixture script exits at once, so a restore would write the marker
    // almost immediately. Poll briefly before asserting it never ran.
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(!marker.exists(), "a resumed attach must restore nothing before its required checkpoint succeeds");
}

/// A failed attach releases the blocker it put on the shared restore.
///
/// A resumed attach records itself as an unresolved owner when it defers the
/// workspace restore, and `release_workspace_lease` does not clear that blocker.
/// A failure after ownership is reconciled has nothing left to protect, so the
/// blocker must go: otherwise every other session that shares the runtime stays
/// blocked until this session resumes again or the runtime shuts down. The
/// holder lease stands in for such a session, keeping the runtime and its debt
/// alive after the failed attach has released its own lease.
#[tokio::test]
async fn a_failed_attach_releases_its_workspace_restore_blocker() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the failed-attach release test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = seed_restorable_background_ledger(&state_dir, "failed-attach-release");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let workspace = AcpSessionWorkspace::from_client_cwd(&session_workspace)
        .await
        .expect("build the ACP session workspace from the session directory");
    // The runtime can be built before any session resumes, so a lease taken
    // here keeps it, and the debt it carries, alive across the failed attach.
    let holder_session = acp::SessionId::new("background-failed-attach-holder");
    let holder = agent
        .workspace_runtime_for(&workspace, Some(&holder_session))
        .await
        .expect("hold the shared workspace runtime");
    let workspace_runtime = Arc::clone(holder.runtime());

    let session_id = acp::SessionId::new("background-failed-attach-session");
    let mut listing = archived_listing("background-failed-attach", &session_workspace, "continue");
    listing.snapshot.messages = vec![SessionMessage::from(&Message::assistant_with_tools(
        String::new(),
        vec![ToolCall::function(
            "deferred-restore-call".to_string(),
            "apply_patch".to_string(),
            "{}".to_string(),
        )],
    ))];
    listing.snapshot.total_messages = 1;
    fs::create_dir(&listing.path).expect("occupy the archive path with a directory");
    let error = match agent.attach_thread_from_listing(&session_id, &workspace, listing).await {
        Ok(_) => panic!("a directory at the archive path must make the required checkpoint fail"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "Failed to persist repaired ACP tool-call history: archive_write_failed",
        "the deadline must fail at the required checkpoint"
    );

    // The failed attach reconciled ownership, so only the holder may still
    // block the debt; its release must be enough to make the restore payable.
    workspace_runtime.release_background_restore(holder_session.0.as_ref());
    assert!(
        workspace_runtime.pending_background_restore_is_payable(),
        "a failed attach whose ownership is reconciled must not hold the shared restore"
    );
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the failed attach");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the failed attach");
    assert_eq!(
        persisted["records"][0]["exec_session_id"], "exec-missing",
        "a failed attach must not restore a child before its checkpoint: {persisted}"
    );

    // A later acquisition reuses the runtime and claims the debt the failure
    // kept, rather than waiting for a resume of this session that may never
    // come.
    let retry = agent
        .workspace_runtime_for(&workspace, None)
        .await
        .expect("a later acquisition must reuse the runtime that carries the debt");
    assert!(
        Arc::ptr_eq(retry.runtime(), &workspace_runtime),
        "the retry must reuse the runtime that carries the debt"
    );
    assert!(
        !workspace_runtime.pending_background_restore_is_payable(),
        "a successful pass must discharge the debt"
    );
    drop(retry);
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(marker.exists(), "the released debt must be restored by the next acquisition");
    drop(holder);
}

/// A restore that fails keeps its obligation for a later attempt.
///
/// The first pass cannot take the ledger lock, so it restores nothing. The debt
/// must stay owed afterwards, or the ownerless row would never come back; once
/// the obstruction clears, a later acquisition retries the pass and the restored
/// launch reaches the ledger and the fixture's marker.
#[tokio::test]
async fn a_failed_workspace_restore_stays_owed_and_a_later_attempt_restores() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("create temporary workspace for the retried-restore test");
    let launch_workspace = temp.path();
    let session_workspace = launch_workspace.join("session-workspace");
    fs::create_dir_all(&session_workspace).expect("create the session workspace");
    let marker = session_workspace.join("background-restored-marker");
    write_background_restore_fixture_with_marker(&session_workspace, Some(&marker));
    let state_dir = session_workspace.join(diapason_commons::workspace_dir_name()).join("state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    let ledger_path = seed_restorable_background_ledger(&state_dir, "retried");
    let lock_path = state_dir.join("background_subagents.lock");
    fs::create_dir(&lock_path).expect("occupy the ledger lock path with a directory");
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    let agent = build_agent_with_vt_config(launch_workspace, "duck", Some(&vt_config)).await;
    let session_id = acp::SessionId::new("background-retried-restore-session");
    let listing = archived_listing("background-retried-restore", &session_workspace, "continue");
    let handle = agent
        .attach_thread_from_listing(
            &session_id,
            &AcpSessionWorkspace::from_client_cwd(&session_workspace)
                .await
                .expect("build the ACP session workspace from the session directory"),
            listing,
        )
        .await
        .expect("attach must continue when background owner migration fails");
    let workspace_runtime = {
        let data = handle.data.lock().expect("lock the attached session data");
        data.workspace_runtime
            .clone()
            .expect("attach must retain its workspace runtime")
    };

    // This acquire models the resumed attach's release once its migration is
    // reconciled: the ledger is writable again, but the restore's own lock
    // attempt still fails, because the sidecar path is occupied.
    fs::remove_dir(&lock_path).expect("clear the ledger lock obstruction");
    workspace_runtime.release_background_restore(session_id.0.as_ref());
    assert!(
        workspace_runtime.pending_background_restore_is_payable(),
        "a reconciled release makes the owed restore payable"
    );
    fs::create_dir(&lock_path).expect("occupy the ledger lock path again");
    agent.run_owed_workspace_background_restore(&workspace_runtime).await;
    assert!(
        workspace_runtime.pending_background_restore_is_payable(),
        "a failed restore must keep the obligation instead of discharging it"
    );
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the failed restore");
    let persisted: serde_json::Value =
        serde_json::from_str(&ledger).expect("parse the ledger after the failed restore");
    assert_eq!(
        persisted["records"][0]["exec_session_id"], "exec-missing",
        "the failed pass must not persist a launch: {persisted}"
    );

    // A later acquisition reuses the runtime and retries the pass; this time the
    // lock is available, so the row is restored and the debt is discharged.
    fs::remove_dir(&lock_path).expect("clear the ledger lock obstruction");
    let workspace = AcpSessionWorkspace::from_client_cwd(&session_workspace)
        .await
        .expect("build the retrying ACP session workspace");
    let retry = agent
        .workspace_runtime_for(&workspace, None)
        .await
        .expect("a later acquisition must reuse the cached runtime");
    assert!(
        Arc::ptr_eq(retry.runtime(), &workspace_runtime),
        "the retry must reuse the runtime that carries the debt"
    );
    assert!(
        !workspace_runtime.pending_background_restore_is_payable(),
        "a successful pass must discharge the debt"
    );
    drop(retry);

    // A restored launch replaces the exec session the ledger names.
    for _ in 0..20 {
        let restored = fs::read_to_string(&ledger_path)
            .ok()
            .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
            .and_then(|ledger| ledger["records"][0]["exec_session_id"].as_str().map(str::to_owned));
        if restored
            .as_deref()
            .is_some_and(|exec_session_id| exec_session_id != "exec-missing")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let ledger = fs::read_to_string(&ledger_path).expect("read the ledger after the retry");
    let persisted: serde_json::Value = serde_json::from_str(&ledger).expect("parse the ledger after the retry");
    assert_ne!(
        persisted["records"][0]["exec_session_id"], "exec-missing",
        "the retried pass must record the launch it restored: {persisted}"
    );
    for _ in 0..20 {
        if marker.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(marker.exists(), "the retried pass must actually execute the restored worker");
}

/// Seeds one ownerless `background-demo` row a workspace restore would relaunch.
///
/// The row names an exec session this process does not know, so a restore
/// observes it as exited and relaunches the fixture script. It carries no owner,
/// which is exactly the row a workspace-scope restore is authorized to take.
fn seed_restorable_background_ledger(state_dir: &Path, suffix: &str) -> std::path::PathBuf {
    let ledger_path = state_dir.join("background_subagents.json");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "session_id": format!("session-background-demo-{suffix}"),
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    fs::write(&ledger_path, serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger"))
        .expect("seed the background ledger");
    ledger_path
}

/// Writes the `background-demo` spec and a script that exits at once.
///
/// The demo agent launches its script directly, so a restore attempt needs both
/// a discoverable spec and an executable. The script must not sleep: the
/// regression proves a failed attach never launches it, and it must leave no
/// child behind if it ever does.
fn write_background_restore_fixture(workspace: &Path) {
    write_background_restore_fixture_with_marker(workspace, None);
}

/// Writes the same fixture, optionally recording that the script ran.
///
/// A marker lets a regression observe an erroneous restore even when the save
/// that would record the launch fails; the ledger alone cannot show it. The
/// ownership regressions reuse it so their worker is genuinely runnable.
pub(super) fn write_background_restore_fixture_with_marker(workspace: &Path, marker: Option<&Path>) {
    let agent_dir = workspace.join(".diapason/agents");
    fs::create_dir_all(&agent_dir).expect("create background fixture agent directory");
    fs::write(
        agent_dir.join("background-demo.md"),
        r#"---
name: background-demo
description: Background restore checkpoint fixture.
tools:
  - command_session
background: true
maxTurns: 2
initialPrompt: Report readiness once.
---

Run the managed background fixture.
"#,
    )
    .expect("write background fixture agent");
    let scripts_dir = workspace.join("scripts");
    fs::create_dir_all(&scripts_dir).expect("create background fixture scripts directory");
    let script = scripts_dir.join("demo-background-subagent.sh");
    let body = match marker {
        Some(marker) => {
            format!("#!/bin/sh\nprintf 'background-restored\\n'\nprintf 'restored\\n' > \"{}\"\n", marker.display())
        }
        None => "#!/bin/sh\nprintf 'background-restored\\n'\n".to_string(),
    };
    fs::write(&script, body).expect("write background fixture script");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(&script).expect("background fixture script metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("make background fixture script executable");
    }
}

/// Clears a ledger-lock obstruction while the workspace runtime is prepared.
///
/// The resume-site owner migration runs before the workspace runtime is built,
/// so a lock obstructed at that point fails that migration. Clearing the
/// obstruction here lets the workspace controller load the still-ownerless
/// ledger and lets the session controller's retry migrate the row, which is the
/// ordering the resumed attach has to reconcile.
pub(super) struct ClearLedgerLockOnBuild {
    pub(super) lock_path: std::path::PathBuf,
}

impl WorkspaceFileOpsToolConstruction for ClearLedgerLockOnBuild {
    fn construct(
        &self,
        workspace_root: &Path,
        zed_config: &AgentClientProtocolZedConfig,
    ) -> anyhow::Result<Option<FileOpsTool>> {
        if self.lock_path.exists() {
            fs::remove_dir(&self.lock_path).expect("clear the ledger lock obstruction");
        }
        Ok(zed_config.tools.list_files.then(|| {
            FileOpsTool::new(
                workspace_root.to_path_buf(),
                Arc::new(GrepSearchManager::new(workspace_root.to_path_buf())),
            )
        }))
    }
}

/// Seeds one ownerless row this session launched with a launch this process
/// does not know, so a restore observes it as exited.
pub(super) fn seed_launched_background_ledger(state_dir: &Path, parent_session_id: &str) -> std::path::PathBuf {
    let ledger_path = state_dir.join("background_subagents.json");
    let seeded_ledger = serde_json::json!({
        "records": [{
            "id": "background-background-demo",
            "agent_name": "background-demo",
            "display_label": "background-demo",
            "description": "background demo",
            "source": "builtin",
            "color": null,
            "parent_session_id": parent_session_id,
            "session_id": "session-background-demo-restored",
            "exec_session_id": "exec-missing",
            "desired_enabled": true,
            "status": "stopped",
            "created_at": "2026-08-28T00:00:00Z",
            "updated_at": "2026-08-28T00:00:00Z",
            "started_at": null,
            "ended_at": "2026-08-28T00:00:00Z",
            "pid": null,
            "prompt": "restore me",
            "summary": null,
            "error": null,
            "archive_path": null,
            "transcript_path": null,
            "max_turns": null,
            "model_override": null,
            "reasoning_override": null,
            "restart_attempts": 0
        }]
    });
    fs::write(&ledger_path, serde_json::to_vec(&seeded_ledger).expect("serialize the seeded background ledger"))
        .expect("seed the background ledger");
    ledger_path
}

/// Writes the demo fixture with a script that records one line per launch.
pub(super) fn write_launch_counting_fixture(workspace: &Path, launches: &Path) {
    write_background_restore_fixture_with_marker(workspace, None);
    let script = workspace.join("scripts/demo-background-subagent.sh");
    let body =
        format!("#!/bin/sh\nprintf 'background-restored\\n'\nprintf 'launch\\n' >> \"{}\"\n", launches.display());
    fs::write(&script, body).expect("write the launch-counting fixture script");
}

/// The number of launches the fixture script has recorded.
pub(super) fn launch_count(launches: &Path) -> usize {
    fs::read_to_string(launches).map_or(0, |recorded| recorded.lines().count())
}

/// Builds the settings a resumed-background test needs.
pub(super) fn restored_background_config() -> diapason_core::config::DiapasonConfig {
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    vt_config
}
