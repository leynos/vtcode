//! Owner-scoped background records: persisted round-trips, per-owner
//! isolation of the shared ledger, and owner-qualified record ids.
use super::*;

#[test]
fn persisted_background_owner_round_trips_to_public_status() {
    let record: PersistedBackgroundRecord = serde_json::from_value(serde_json::json!({
        "id": "background-worker",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "owner_session_id": "parent-session",
        "session_id": "child-session",
        "exec_session_id": "exec-session",
        "desired_enabled": true,
        "status": "running",
        "created_at": "2026-08-28T00:00:00Z",
        "updated_at": "2026-08-28T00:00:00Z",
        "started_at": "2026-08-28T00:00:00Z",
        "ended_at": null,
        "pid": 42,
        "prompt": "work",
        "summary": null,
        "error": null,
        "archive_path": null,
        "transcript_path": null,
        "max_turns": 4,
        "model_override": null,
        "reasoning_override": null,
        "restart_attempts": 0
    }))
    .expect("deserialize persisted record");

    let json = serde_json::to_string(&record).expect("serialize persisted record");
    let decoded: PersistedBackgroundRecord = serde_json::from_str(&json).expect("deserialize persisted record");
    let runtime = BackgroundRecord::from_persisted(decoded);

    assert_eq!(runtime.owner_session_id.as_deref(), Some("parent-session"));
    assert_eq!(runtime.build_status_entry().owner_session_id.as_deref(), Some("parent-session"));
}

#[test]
fn legacy_background_record_without_owner_is_loadable_and_ownerless() {
    let mut value = serde_json::json!({
        "id": "background-worker",
        "agent_name": "worker",
        "display_label": "worker",
        "description": "worker",
        "source": "builtin",
        "color": null,
        "session_id": "child-session",
        "exec_session_id": "exec-session",
        "desired_enabled": true,
        "status": "running",
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
    });
    assert!(value.as_object_mut().expect("object").remove("owner_session_id").is_none());

    let decoded: PersistedBackgroundRecord = serde_json::from_value(value).expect("legacy record remains loadable");
    let runtime = BackgroundRecord::from_persisted(decoded);
    assert!(runtime.owner_session_id.is_none());
    assert!(runtime.build_status_entry().owner_session_id.is_none());
}

#[tokio::test]
async fn owner_scoped_controller_loads_only_exact_owner_and_cli_remains_unscoped() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".vtcode/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let records = vec![
        persisted_background_record_for_owner("owned", Some("session-a")),
        persisted_background_record_for_owner("foreign", Some("session-b")),
        persisted_background_record_for_owner("legacy", None),
    ];
    std::fs::write(
        state_dir.join("background_subagents.json"),
        serde_json::to_string(&PersistedBackgroundState { records }).expect("serialize state"),
    )
    .expect("write state");

    let scoped = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()),
        Some("session-a"),
    )
    .await
    .expect("scoped controller");
    let scoped_entries = scoped.background_status_entries().await;
    assert_eq!(
        scoped_entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        ["background-worker@session-a"]
    );

    {
        let mut state = scoped.state.write().await;
        state
            .background_children
            .get_mut("background-worker@session-a")
            .expect("owned record")
            .summary = Some("updated by session-a".to_string());
    }
    scoped.save_background_state().await.expect("save scoped state");
    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("background_subagents.json"))
            .expect("read persisted background_subagents state"),
    )
    .expect("persisted state");
    let records = persisted["records"].as_array().expect("records array");
    assert_eq!(records.len(), 3);
    let by_id = records
        .iter()
        .map(|record| (record["id"].as_str().expect("record id"), record))
        .collect::<std::collections::HashMap<_, _>>();
    assert_eq!(by_id["background-worker@session-a"]["summary"], "updated by session-a");
    // Records owned by another session, and legacy ownerless records, are
    // retained verbatim until their own owner rewrites them.
    assert_eq!(by_id["foreign"]["owner_session_id"], "session-b");
    assert!(by_id["legacy"]["owner_session_id"].is_null());

    let unscoped = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("unscoped controller");
    let unscoped_ids = unscoped
        .background_status_entries()
        .await
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert_eq!(
        unscoped_ids,
        [
            "background-worker",
            "background-worker@session-a",
            "background-worker@session-b"
        ]
    );
}

/// Two sessions may launch the same agent in one workspace. Their records are
/// persisted with the same logical agent but must never collapse into one map
/// entry, because the surviving unscoped save would delete the other owner's
/// task from the shared ledger.
#[tokio::test]
async fn sibling_owners_keep_distinct_records_for_the_same_agent() {
    let temp = TempDir::new().expect("tempdir");
    let state_dir = temp.path().join(".vtcode/state");
    std::fs::create_dir_all(&state_dir).expect("state dir");
    let records = vec![
        persisted_background_record_for_owner("background-worker", Some("session-a")),
        persisted_background_record_for_owner("background-worker", Some("session-b")),
    ];
    std::fs::write(
        state_dir.join("background_subagents.json"),
        serde_json::to_string(&PersistedBackgroundState { records }).expect("serialize state"),
    )
    .expect("write state");

    let unscoped = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("unscoped controller");
    let ids = unscoped
        .background_status_entries()
        .await
        .into_iter()
        .map(|entry| entry.id)
        .collect::<Vec<_>>();
    assert_eq!(ids, ["background-worker@session-a", "background-worker@session-b"]);

    unscoped.save_background_state().await.expect("save unscoped state");
    let persisted: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("background_subagents.json"))
            .expect("read persisted background_subagents state"),
    )
    .expect("persisted state");
    let owners = persisted["records"]
        .as_array()
        .expect("records array")
        .iter()
        .map(|record| record["owner_session_id"].as_str().expect("owner").to_string())
        .collect::<Vec<_>>();
    assert_eq!(owners, ["session-a", "session-b"], "a workspace-global save must persist both owners' records");
}

#[cfg(unix)]
async fn spawn_background_demo(owner_session_id: Option<&str>) -> BackgroundSubprocessEntry {
    let temp = TempDir::new().expect("tempdir");
    write_test_background_subagent(temp.path());
    let mut cfg = VTCodeConfig::default();
    cfg.subagents.background.enabled = true;
    let controller = SubagentController::new_with_background_owner(
        test_controller_config(temp.path().to_path_buf(), cfg),
        owner_session_id,
    )
    .await
    .expect("controller");

    controller
        .spawn_background_subprocess(SpawnBackgroundSubprocessRequest {
            agent_type: Some("background-demo".to_string()),
            ..SpawnBackgroundSubprocessRequest::default()
        })
        .await
        .expect("background subprocess")
}

#[cfg(unix)]
#[tokio::test]
async fn managed_background_records_capture_the_owning_scope() {
    let scoped = spawn_background_demo(Some("session-a")).await;
    assert_eq!(scoped.owner_session_id.as_deref(), Some("session-a"));
    assert_eq!(scoped.id, "background-background-demo@session-a");

    let unscoped = spawn_background_demo(None).await;
    assert!(
        unscoped.owner_session_id.is_none(),
        "an unscoped controller manages workspace-global records and must not tag them with a per-run owner"
    );
    assert_eq!(unscoped.id, "background-background-demo");
}

#[test]
fn owner_scoped_record_ids_distinguish_owners_and_keep_the_global_id() {
    assert_eq!(background_record_id_for_owner("worker", Some("session-a")), "background-worker@session-a");
    assert_ne!(
        background_record_id_for_owner("worker", Some("session-a")),
        background_record_id_for_owner("worker", Some("session-b"))
    );
    assert_eq!(background_record_id_for_owner("worker", None), background_record_id("worker"));
    assert_eq!(
        background_record_id_for_owner("worker", Some("")),
        background_record_id("worker"),
        "an empty owner cannot produce a distinct scope"
    );
}
