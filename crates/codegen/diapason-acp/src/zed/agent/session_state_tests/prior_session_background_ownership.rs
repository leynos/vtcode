//! A new ACP session must not adopt another session's background work.
use super::*;
use assert_fs::TempDir;
use diapason_core::subagents::TaskOwner;

/// A new session may not restore a background record another session owns.
///
/// The seeded row names a different owner, so neither the fresh session's
/// controller nor the workspace scope may claim it: only the owning session can
/// restore it, and a new session cannot become that owner.
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
                "agent_name": "worker",
                "display_label": "worker",
                "description": "worker",
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
    let response = agent.new_session(acp::NewSessionRequest::new(temp.path())).await.unwrap();
    let session = agent.session_handle(&response.session_id).unwrap().unwrap();
    let controller = agent
        .session_subagent_controller(&session)
        .unwrap()
        .expect("per-session subagent controller");

    assert!(controller.background_status_entries(TaskOwner::Workspace).await.is_empty());
}
