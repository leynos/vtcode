//! A new ACP session must not adopt another session's background work.
use super::*;
use assert_fs::TempDir;
use diapason_core::subagents::{BackgroundSubprocessStatus, TaskOwner, background_record_id_for_owner};

/// A new session may not restore a background record another session owns.
///
/// The seeded row names a different owner, so neither the fresh session's
/// controller nor the workspace scope may claim it: only the owning session can
/// restore it, and a new session cannot become that owner. Both boundaries are
/// asserted, because the two scopes resolve ownership independently: a session
/// controller answers for its own records, and the workspace controller answers
/// for the ownerless ones a resume may leave behind. The row names an agent that
/// resolves to no spec, so a restore attempt fails before it can spawn a process.
#[tokio::test]
async fn new_session_does_not_restore_a_background_record_owned_by_another_session() {
    let temp = TempDir::new().unwrap();
    let state_dir = temp.path().join(".diapason/state");
    fs::create_dir_all(&state_dir).unwrap();
    fs::write(
        state_dir.join("background_subagents.json"),
        serde_json::json!({
            "records": [{
                "id": "foreign-worker",
                "agent_name": "retired-worker",
                "display_label": "retired-worker",
                "description": "retired-worker",
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
    let agent = build_agent_with_vt_config(temp.path(), "duck", Some(&vt_config)).await;
    // The launch runtime is built from the same workspace before any session
    // exists, so its controller is the scope a fresh attach would fall back to.
    let launch_controller = agent
        .local_tool_registry
        .subagent_controller()
        .expect("the launch workspace controller");
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
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
        background_record_id_for_owner("retired-worker", Some("prior-session")),
        "the loaded record keeps the owner-scoped id of the session it belongs to"
    );
    assert_eq!(
        foreign.owner_session_id.as_deref(),
        Some("prior-session"),
        "the workspace scope must leave the owning session's record with its owner"
    );
    assert_eq!(
        foreign.exec_session_id, "exec-session",
        "the workspace scope must not relaunch a record a prior session owns"
    );
    assert_eq!(
        foreign.status,
        BackgroundSubprocessStatus::Stopped,
        "a restore attempt would settle this record Error while keeping its launch id"
    );
}
