use super::*;

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
        .expect("the session cancels its own record through the legacy alias")
        .into_entry();
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
