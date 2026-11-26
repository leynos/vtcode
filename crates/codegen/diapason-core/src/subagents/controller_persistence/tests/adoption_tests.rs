//! Adopting ownerless ledger rows onto their owning session.
//!
//! Declared by `controller_persistence/tests/mod.rs`. Pins which rows a
//! controller may adopt and rekey, including that an owned row for a different
//! launch is never adopted. It provides the workspace fixtures the
//! migration-evidence cases in `launch_precedence_tests.rs` reuse.
use super::super::*;
use crate::config::DiapasonConfig;
use crate::config::types::ReasoningEffortLevel;
use crate::subagents::background::{background_record_id_for_owner, load_background_state, persist_background_state};
use crate::subagents::{SubagentController, SubagentControllerConfig, TaskOwner};
use crate::tools::exec_session::ExecSessionManager;
use crate::tools::registry::PtySessionManager;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Builds a controller for a temporary workspace.
///
/// The fixture owns no provider access: these tests exercise ledger loads,
/// saves and cancellations, which never reach a model.
pub(super) fn ownership_controller_config(workspace_root: PathBuf) -> SubagentControllerConfig {
    let pty_sessions = PtySessionManager::new(workspace_root.clone(), DiapasonConfig::default().pty.clone());
    let exec_sessions = ExecSessionManager::new(workspace_root.clone(), pty_sessions.clone());
    SubagentControllerConfig {
        workspace_root,
        parent_session_id: "parent-session".to_string(),
        parent_model: "gpt-5.6-sol".to_string(),
        parent_provider: "openai".to_string(),
        parent_reasoning_effort: ReasoningEffortLevel::Medium,
        api_key: "test-key".to_string(),
        vt_cfg: DiapasonConfig::default(),
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

/// Renders the ownerless row an older build wrote for a background launch.
pub(super) fn legacy_row(parent_session_id: &str) -> PersistedBackgroundRecord {
    serde_json::from_value(serde_json::json!({
        "id": "background-worker",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "owner_session_id": null,
        "session_id": "child-session",
        "parent_session_id": parent_session_id,
        "exec_session_id": "exec-launch",
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
    }))
    .expect("legacy persisted record")
}

/// Writes a ledger the resume migration has already given to `owner`.
async fn migrate_ledger_to(workspace: &Path, owner: &str) {
    let mut state = load_background_state(workspace).await.expect("read seeded ledger");
    let record = state.records.first_mut().expect("the seeded record");
    record.owner_session_id = Some(owner.to_string());
    persist_background_state(workspace, state.records)
        .await
        .expect("write migrated ledger");
}

/// A migrated launch is adopted, saved once, and cannot be resurrected after
/// the owning session stops it.
///
/// This is the ordering the ACP launch runtime creates: the workspace
/// controller loads the ownerless row before the session migrates it, so its
/// in-memory copy is stale. Saving that copy without reconciliation would write
/// an ownerless record beside the migrated one, which a later restore could
/// relaunch after the session cancelled the task.
#[tokio::test]
async fn a_migrated_launch_is_adopted_and_never_revived_by_a_workspace_save() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    let before = workspace_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(before.len(), 1, "the workspace controller loads the ownerless row");
    assert_eq!(before[0].owner_session_id, None, "the loaded copy is still ownerless");
    assert_eq!(before[0].exec_session_id, "exec-launch", "the launch must not be replaced");

    migrate_ledger_to(temp.path(), owner).await;

    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adopt the migrated launch"),
        1,
        "the stale ownerless copy joins the migrated record"
    );
    workspace_controller
        .save_background_ledger()
        .await
        .expect("save the adopted launch");

    let owned_id = background_record_id_for_owner("worker", Some(owner));
    let entries = workspace_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(entries.len(), 1, "adoption rekeys the record rather than duplicating it");
    assert_eq!(entries[0].id, owned_id, "the record is keyed by its owned canonical id");
    assert_eq!(entries[0].owner_session_id.as_deref(), Some(owner));
    let ledger = load_background_state(temp.path()).await.expect("read the saved ledger");
    assert_eq!(ledger.records.len(), 1, "the workspace save must not write an ownerless duplicate");
    assert_eq!(ledger.records[0].id, owned_id);
    assert_eq!(ledger.records[0].owner_session_id.as_deref(), Some(owner));

    let session_controller = SubagentController::new_with_background_owner(
        ownership_controller_config(temp.path().to_path_buf()),
        Some(owner),
    )
    .await
    .expect("session controller");
    // The stop's outcome is not this test's subject; dropping it as a value
    // keeps the cancellation's effect on the ledger the only thing asserted.
    let _ = session_controller
        .force_cancel_background(TaskOwner::Session(owner), &owned_id)
        .await
        .expect("cancel the owned launch")
        .into_entry();

    workspace_controller
        .save_background_ledger()
        .await
        .expect("save after the session stopped the launch");

    let ledger = load_background_state(temp.path())
        .await
        .expect("read the ledger after cancellation");
    assert_eq!(ledger.records.len(), 1, "a cancelled launch must leave nothing restorable");
    assert_eq!(ledger.records[0].owner_session_id.as_deref(), Some(owner));
    let persisted = serde_json::to_value(&ledger.records[0]).expect("render the persisted record");
    assert_eq!(
        persisted["desired_enabled"],
        serde_json::json!(false),
        "a workspace save must not revive the launch the session stopped"
    );
}

/// A failed migration adopts nothing.
///
/// The ledger still names the row ownerless, so the launch controller must not
/// invent the owner the migration failed to write; a later retry is the only
/// step that may attribute it.
#[tokio::test]
async fn adoption_waits_for_a_ledger_that_records_the_owner() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");

    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "an ownerless ledger must not be attributed by the controller"
    );
    let entries = workspace_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].owner_session_id, None, "the copy stays ownerless as the ledger says");
}

/// A different exec session means the ledger holds another launch.
///
/// Adoption joins an in-memory ownerless copy to the owned row for the same
/// launch. A canonical id alone is not the launch identity: a controller that
/// loaded an ownerless copy before a different owned launch was persisted
/// would otherwise rekey that stale copy onto the ledger row and save it over
/// the launch the session actually owns.
#[tokio::test]
async fn adoption_ignores_an_owned_row_for_a_different_launch() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    assert_eq!(
        workspace_controller.background_status_entries(TaskOwner::Workspace).await.len(),
        1,
        "the workspace controller loads the ownerless copy"
    );

    // The session persisted its own launch under the same canonical id, but
    // that row names a different exec session than the copy loaded above.
    let mut ledger = load_background_state(temp.path()).await.expect("read seeded ledger");
    let row = ledger.records.first_mut().expect("the seeded record");
    row.owner_session_id = Some(owner.to_string());
    row.exec_session_id = "exec-replacement".to_string();
    row.canonicalize_id();
    persist_background_state(temp.path(), ledger.records)
        .await
        .expect("write the migrated ledger");

    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "an owned row for another launch is not the launch this copy loaded"
    );
    let entries = workspace_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].owner_session_id, None, "the copy keeps the owner the ledger gave it");
    assert_eq!(entries[0].exec_session_id, "exec-launch", "the copy keeps the launch it loaded");
}

/// An owner-scoped controller adopts nothing.
///
/// It already loads only its own rows, so a workspace-wide adoption pass would
/// give it authority over records it never had.
#[tokio::test]
async fn only_an_unscoped_controller_adopts_rows() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    persist_background_state(temp.path(), vec![legacy_row(owner)])
        .await
        .expect("seed the ownerless ledger");
    migrate_ledger_to(temp.path(), owner).await;

    let session_controller = SubagentController::new_with_background_owner(
        ownership_controller_config(temp.path().to_path_buf()),
        Some(owner),
    )
    .await
    .expect("session controller");

    assert_eq!(
        session_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "an owner-scoped controller has no ownerless copies to adopt"
    );
}

/// An empty exec session cannot make an ownerless copy adopt an owned row.
///
/// A legacy ledger can hold both an ownerless row and an owned row for the same
/// agent that name no exec session at all. Those rows are different child
/// sessions: a shared empty id is not a launch identity, so the ownerless copy
/// must not be rekeyed onto the owned record. Adopting it would give the copy
/// an owner the migration never wrote and, because the owned canonical id is
/// already taken by the row that really is the owned launch, rekey it over that
/// row on the next save.
#[tokio::test]
async fn adoption_ignores_rows_that_name_no_launch() {
    let temp = TempDir::new().expect("temporary workspace");
    let owner = "session-a";
    let mut ownerless = legacy_row(owner);
    ownerless.exec_session_id = String::new();
    let mut owned = legacy_row(owner);
    owned.owner_session_id = Some(owner.to_string());
    owned.exec_session_id = String::new();
    owned.canonicalize_id();
    persist_background_state(temp.path(), vec![ownerless, owned])
        .await
        .expect("seed a ledger with one ownerless and one owned row");

    let workspace_controller = SubagentController::new(ownership_controller_config(temp.path().to_path_buf()))
        .await
        .expect("workspace controller");
    assert_eq!(
        workspace_controller
            .adopt_persisted_background_owner(owner)
            .await
            .expect("adoption attempt"),
        0,
        "a copy that names no launch must not adopt an owned row that names none either"
    );
    let entries = workspace_controller.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(entries.len(), 2, "both rows survive the adoption pass: {entries:?}");
    assert!(
        entries.iter().any(|entry| entry.owner_session_id.is_none()),
        "the ownerless row keeps the owner the ledger gave it"
    );
}
