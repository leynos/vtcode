//! Background ledger ownership across an ACP session resume.
//!
//! The ACP launch workspace runtime is built before any session resumes, so it
//! loads the ledger while a legacy row is still ownerless. A resumed session
//! migrates that row to itself, and the launch controller must then adopt the
//! row it already holds instead of restoring it as workspace work or writing
//! its stale ownerless copy back beside the migrated record.
//!
//! These regressions pin that ordering at the ACP boundary. Deterministic
//! fixtures only: the seeded row names an agent that resolves to no subagent
//! spec, so any restore attempt fails before spawning a process. The same
//! fixture pins that a new session never migrates: its fresh owner can match
//! no persisted row, so the migration may not take the ledger lock at all.
use super::archive::archived_listing;
use super::*;
use crate::zed::agent::test_log_capture::capturing_async;
use crate::zed::session::AcpSessionWorkspace;
use assert_fs::TempDir;
use diapason_core::subagents::{BackgroundSubprocessStatus, background_record_id_for_owner};
use tracing_subscriber::filter::LevelFilter;

/// The agent the seeded legacy row names.
///
/// It must not resolve to a real subagent spec. A restore attempt then fails
/// while resolving the agent, before any process exists, which keeps these
/// regressions deterministic without depending on model access.
const LEGACY_AGENT: &str = "retired-worker";

/// The exec session the seeded row names, standing in for its live process.
const LEGACY_EXEC_SESSION: &str = "exec-legacy-launch";

/// Builds the subagent configuration that makes the ordering observable.
fn auto_restore_config() -> diapason_core::config::DiapasonConfig {
    let mut vt_config = diapason_core::config::DiapasonConfig::default();
    vt_config.subagents.enabled = true;
    vt_config.subagents.background.enabled = true;
    vt_config.subagents.background.auto_restore = true;
    vt_config
}

/// Writes the ownerless ledger row a pre-ownership build recorded.
fn seed_legacy_ledger(workspace: &Path, parent_session_id: &str) {
    let state_dir = workspace.join(".diapason/state");
    fs::create_dir_all(&state_dir).expect("create the background state directory");
    fs::write(
        state_dir.join("background_subagents.json"),
        serde_json::json!({
            "records": [{
                "id": format!("background-{LEGACY_AGENT}"),
                "agent_name": LEGACY_AGENT,
                "display_label": LEGACY_AGENT,
                "description": LEGACY_AGENT,
                "source": "builtin",
                "color": null,
                "owner_session_id": null,
                "session_id": "child-session",
                "parent_session_id": parent_session_id,
                "exec_session_id": LEGACY_EXEC_SESSION,
                "desired_enabled": true,
                "status": "running",
                "created_at": "2026-08-28T00:00:00Z",
                "updated_at": "2026-08-28T00:00:00Z",
                "started_at": "2026-08-28T00:00:00Z",
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
    .expect("seed the ownerless background ledger");
}

/// Reads the persisted ledger records as raw JSON, so a regression can assert on
/// fields the controller keeps private.
fn read_ledger_records(workspace: &Path) -> Vec<serde_json::Value> {
    let raw = fs::read_to_string(workspace.join(".diapason/state/background_subagents.json"))
        .expect("read the background ledger");
    let state: serde_json::Value = serde_json::from_str(&raw).expect("parse the background ledger");
    state["records"].as_array().cloned().expect("background ledger records")
}

/// A row the launch workspace already loaded must end owned by the resumed
/// session, and the launch controller must adopt it rather than relaunch it.
#[tokio::test]
async fn resumed_session_owns_the_legacy_row_the_launch_workspace_loaded() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("temporary workspace for background ownership test");
    let session_id = acp::SessionId::new("resumed-background-owner");
    let owner = session_id.0.as_ref();
    seed_legacy_ledger(temp.path(), owner);
    let vt_config = auto_restore_config();

    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let launch_controller = agent
        .local_tool_registry
        .subagent_controller()
        .expect("the launch workspace controller");
    let launch_view = launch_controller.background_status_entries().await;
    assert_eq!(launch_view.len(), 1, "the launch runtime loads the ownerless row");
    assert_eq!(launch_view[0].owner_session_id, None, "the loaded copy is ownerless before the resume");
    assert_eq!(launch_view[0].exec_session_id, LEGACY_EXEC_SESSION, "loading the row must not replace its launch");

    let listing = archived_listing("resumed-background-owner-archive", temp.path(), "continue");
    let workspace = AcpSessionWorkspace::from_client_cwd(temp.path())
        .await
        .expect("build the ACP session workspace from the temporary directory");
    let attached = agent
        .attach_thread_from_listing(&session_id, &workspace, listing)
        .await
        .expect("resume the session that launched the legacy row");

    let owned_id = background_record_id_for_owner(LEGACY_AGENT, Some(owner));
    let records = read_ledger_records(temp.path());
    assert_eq!(records.len(), 1, "the resume must not duplicate the launch");
    assert_eq!(records[0]["owner_session_id"], owner, "the resumed session must own the row it launched");
    assert_eq!(
        records[0]["exec_session_id"], LEGACY_EXEC_SESSION,
        "the session must adopt the existing launch instead of starting another"
    );

    let launch_view = launch_controller.background_status_entries().await;
    assert_eq!(launch_view.len(), 1, "adoption rekeys the stale copy rather than adding one");
    assert_eq!(launch_view[0].id, owned_id, "the adopted record takes its owned canonical id");
    assert_eq!(
        launch_view[0].owner_session_id.as_deref(),
        Some(owner),
        "the launch controller must adopt the migrated row instead of holding it ownerless"
    );
    assert_eq!(
        launch_view[0].exec_session_id, LEGACY_EXEC_SESSION,
        "adoption must leave the launch identity untouched"
    );
    assert!(
        launch_controller
            .restore_legacy_background_subagents()
            .await
            .expect("workspace restore pass after the resume")
            .is_empty(),
        "a row the resumed session owns must never be restored as workspace work"
    );

    let session = agent
        .session_handle(&session_id)
        .expect("look up the resumed session")
        .expect("the resumed session must be registered");
    assert!(Arc::ptr_eq(&attached.data, &session.data), "the resume must register the handle it attached");
    let session_controller = agent
        .session_subagent_controller(&session)
        .expect("select the session controller")
        .expect("the resumed session owns a subagent controller");
    let session_view = session_controller.background_status_entries().await;
    assert_eq!(session_view.len(), 1, "the session controller adopts the migrated row");
    assert_eq!(session_view[0].id, owned_id);
    assert_eq!(session_view[0].exec_session_id, LEGACY_EXEC_SESSION);
}

/// A resumed session must reuse the migrated launch: the launch scope may not
/// restore or refresh it into a replacement.
///
/// The workspace runtime can be built before any session resumes, so its
/// controller may hold an ownerless copy of a row the migration has since
/// given to this session. Adoption must finish before the session controller
/// is built, so neither scope can act on the stale copy and start a second
/// process under the same record.
#[tokio::test]
async fn resumed_session_reuses_the_migrated_launch_instead_of_spawning_a_replacement() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let temp = TempDir::new().expect("temporary workspace for the launch-reuse test");
    let session_id = acp::SessionId::new("resumed-launch-reuse");
    let owner = session_id.0.as_ref();
    seed_legacy_ledger(temp.path(), owner);
    let vt_config = auto_restore_config();
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    let launch_controller = agent
        .local_tool_registry
        .subagent_controller()
        .expect("the launch workspace controller");

    let listing = archived_listing("resumed-launch-reuse-archive", temp.path(), "continue");
    let workspace = AcpSessionWorkspace::from_client_cwd(temp.path())
        .await
        .expect("build the ACP session workspace from the temporary directory");
    drop(
        agent
            .attach_thread_from_listing(&session_id, &workspace, listing)
            .await
            .expect("resume the session that launched the legacy row"),
    );

    let owned_id = background_record_id_for_owner(LEGACY_AGENT, Some(owner));
    let launch_view = launch_controller.background_status_entries().await;
    assert_eq!(launch_view.len(), 1, "the launch scope must hold one record for the migrated launch");
    assert_eq!(launch_view[0].id, owned_id, "the launch scope adopts the migrated record");
    assert_eq!(launch_view[0].owner_session_id.as_deref(), Some(owner));
    assert_eq!(
        launch_view[0].exec_session_id, LEGACY_EXEC_SESSION,
        "adoption must keep the launch the ledger already records"
    );
    assert!(
        launch_controller
            .restore_legacy_background_subagents()
            .await
            .expect("a row the resumed session owns must not fail the launch scope's restore")
            .is_empty(),
        "the launch scope must not restore the launch the resumed session owns"
    );
    let after_refresh = launch_controller
        .refresh_background_processes()
        .await
        .expect("refreshing the launch scope must not replace a launch the resumed session owns");
    assert_eq!(after_refresh.len(), 1, "a refresh must not add a replacement launch");
    assert_eq!(
        after_refresh[0].exec_session_id, LEGACY_EXEC_SESSION,
        "the migrated launch identity must survive a launch-scope refresh"
    );

    let records = read_ledger_records(temp.path());
    assert_eq!(records.len(), 1, "the resumed session must reuse the launch the ledger already records");
    assert_eq!(records[0]["owner_session_id"], owner);
    assert_eq!(records[0]["exec_session_id"], LEGACY_EXEC_SESSION, "no replacement launch may be persisted");

    let session = agent
        .session_handle(&session_id)
        .expect("look up the resumed session")
        .expect("the resumed session must be registered");
    let session_controller = agent
        .session_subagent_controller(&session)
        .expect("select the session controller")
        .expect("the resumed session owns a subagent controller");
    let session_view = session_controller.background_status_entries().await;
    assert_eq!(session_view.len(), 1, "the session controller adopts the migrated launch");
    assert_eq!(session_view[0].exec_session_id, LEGACY_EXEC_SESSION);
}

/// A migration that fails must leave the row ownerless and restore nothing
/// under either scope, so a retried attach can still migrate it once.
#[tokio::test]
async fn a_failed_owner_migration_restores_nothing() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let launch_workspace = TempDir::new().expect("launch workspace for the failed migration test");
    let session_workspace = TempDir::new().expect("session workspace for the failed migration test");
    let session_id = acp::SessionId::new("migration-failure-session");
    let owner = session_id.0.as_ref();
    seed_legacy_ledger(session_workspace.path(), owner);
    // The migration takes the ledger's inter-process lock before rewriting it.
    // Occupying the sidecar path with a directory makes that acquisition fail
    // for every user, so the failure does not depend on permission bits and
    // cannot be silently skipped by a privileged test runner.
    fs::create_dir(session_workspace.path().join(".diapason/state/background_subagents.lock"))
        .expect("occupy the ledger lock path with a directory");
    let vt_config = auto_restore_config();

    let agent = build_agent_with_vt_config(launch_workspace.path(), "duck", Some(&vt_config)).await;
    let listing = archived_listing("migration-failure-archive", session_workspace.path(), "continue");
    let workspace = AcpSessionWorkspace::from_client_cwd(session_workspace.path())
        .await
        .expect("build the ACP session workspace from the session directory");
    let attached = agent
        .attach_thread_from_listing(&session_id, &workspace, listing)
        .await
        .expect("a failed owner migration must not fail the resume");

    let records = read_ledger_records(session_workspace.path());
    assert_eq!(records.len(), 1, "a failed migration must not rewrite the ledger");
    assert!(
        records[0]["owner_session_id"].is_null(),
        "a failed migration must not attribute the row to the session"
    );
    assert_eq!(records[0]["exec_session_id"], LEGACY_EXEC_SESSION, "a failed migration must not relaunch the row");
    assert_eq!(
        records[0]["status"], "running",
        "a failed migration must not settle the row a skipped restore would have settled"
    );

    let session = agent
        .session_handle(&session_id)
        .expect("look up the resumed session")
        .expect("the resumed session must be registered");
    assert!(Arc::ptr_eq(&attached.data, &session.data), "the resume must register the handle it attached");
    let session_controller = agent
        .session_subagent_controller(&session)
        .expect("select the session controller")
        .expect("the resumed session owns a subagent controller");
    assert!(
        session_controller.background_status_entries().await.is_empty(),
        "the session controller must adopt nothing the migration did not attribute"
    );
    let runtime = session
        .workspace_runtime()
        .expect("read the resumed session workspace runtime")
        .expect("the resumed session owns a workspace runtime");
    let workspace_controller = runtime
        .local_tool_registry
        .subagent_controller()
        .expect("the resumed workspace controller");
    let workspace_view = workspace_controller.background_status_entries().await;
    assert_eq!(workspace_view.len(), 1, "the workspace controller still sees the ownerless row");
    assert_eq!(
        workspace_view[0].owner_session_id, None,
        "the workspace controller must not take ownership the migration did not write"
    );
    assert_eq!(
        workspace_view[0].exec_session_id, LEGACY_EXEC_SESSION,
        "the deferred restore must not relaunch the ownerless row"
    );
    assert_eq!(
        workspace_view[0].status,
        BackgroundSubprocessStatus::Running,
        "a restore attempt would settle this row Error while keeping its launch id"
    );
}

/// A new session must not attempt the owner migration.
///
/// Its owner is a fresh identifier, so no persisted row can carry it, and the
/// resume path migrates before it restores. Migrating anyway would take the
/// ledger's inter-process lock on every `session/new`, so a workspace whose
/// lock sidecar is unavailable would report a degraded setup for a session that
/// owns nothing.
#[tokio::test]
async fn a_new_session_opens_with_the_ledger_lock_unavailable() {
    let _history_test_lock = HISTORY_TEST_LOCK.lock().await;
    let _history_settings = HistorySettingsGuard::set(HistoryPersistence::File, None);
    let launch_workspace = TempDir::new().expect("launch workspace for the new-session test");
    let session_workspace = TempDir::new().expect("session workspace for the new-session test");
    // The deterministic pattern from the failed-migration test: occupying the
    // sidecar path with a directory makes any acquisition fail for every user.
    fs::create_dir_all(session_workspace.path().join(".diapason/state"))
        .expect("create the background state directory");
    fs::create_dir(session_workspace.path().join(".diapason/state/background_subagents.lock"))
        .expect("occupy the ledger lock path with a directory");
    let vt_config = auto_restore_config();
    let agent = build_agent_with_vt_config(launch_workspace.path(), "duck", Some(&vt_config)).await;

    let (response, events) =
        capturing_async(LevelFilter::DEBUG, agent.new_session(acp::NewSessionRequest::new(session_workspace.path())))
            .await;
    let response = response.expect("a new session must open even when the ledger lock is unavailable");

    let captured = events.join("\n");
    assert!(
        !captured.contains("background_owner_migration_failed"),
        "a fresh session owns no persisted row, so it must not attempt the owner migration: {captured}"
    );
    let session = agent
        .session_handle(&response.session_id)
        .expect("look up the new session")
        .expect("the new session must be registered");
    let controller = agent
        .session_subagent_controller(&session)
        .expect("select the session controller")
        .expect("the new session still owns a subagent controller");
    assert!(
        controller.background_status_entries().await.is_empty(),
        "a fresh session has no background work to report"
    );
}
