//! A new ACP session must not adopt another session's background work.
use super::background_restore::write_background_restore_fixture_with_marker;
use super::*;
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
