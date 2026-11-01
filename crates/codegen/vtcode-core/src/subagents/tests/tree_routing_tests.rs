//! Regressions for nested-controller discovery and settled-outcome preservation.
use super::*;

#[derive(Default)]
struct RecordingObserver {
    children: Mutex<Vec<SubagentStatusEntry>>,
}

impl SubagentProgressObserver for RecordingObserver {
    fn subagent_status_changed(&self, entry: SubagentStatusEntry) {
        self.children.lock().expect("children lock").push(entry);
    }

    fn background_status_changed(&self, _entry: BackgroundSubprocessEntry) {}
}

fn explorer_spec() -> SubagentSpec {
    vtcode_config::builtin_subagents()
        .into_iter()
        .find(|spec| spec.name == "explorer")
        .expect("explorer spec")
}

fn settled_background_record(
    id: &str,
    owner_session_id: &str,
    status: BackgroundSubprocessStatus,
    error: Option<&str>,
) -> BackgroundRecord {
    let timestamp = Utc::now();
    BackgroundRecord {
        id: id.to_string(),
        owner_session_id: Some(owner_session_id.to_string()),
        agent_name: "watcher".to_string(),
        display_label: "Watcher".to_string(),
        description: "Watch the workspace".to_string(),
        source: "workspace".to_string(),
        color: None,
        session_id: format!("session-{id}"),
        parent_session_id: owner_session_id.to_string(),
        exec_session_id: format!("exec-{id}"),
        desired_enabled: false,
        status,
        created_at: timestamp,
        updated_at: timestamp,
        started_at: Some(timestamp),
        ended_at: Some(timestamp),
        pid: Some(4242),
        prompt: "Watch the workspace".to_string(),
        summary: Some("settled".to_string()),
        error: error.map(str::to_string),
        archive_path: None,
        transcript_path: None,
        max_turns: None,
        model_override: None,
        reasoning_override: None,
        restart_attempts: 0,
    }
}

async fn nested_child_controller(workspace_root: PathBuf, parent_session_id: &str) -> Arc<SubagentController> {
    let mut config = test_controller_config(workspace_root, VTCodeConfig::default());
    config.parent_session_id = parent_session_id.to_string();
    config.depth = 1;
    // Mirror `child_controller_for`: nested scopes run under the managed
    // background runtime guard and therefore never own background work.
    config.managed_background_runtime = true;
    Arc::new(SubagentController::new(config).await.expect("nested controller"))
}

#[tokio::test]
async fn descendant_controllers_include_only_attached_child_scopes() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("parent controller");
    let spec = explorer_spec();
    let nested = nested_child_controller(temp.path().to_path_buf(), "child-session").await;

    {
        let mut state = parent.state.write().await;
        state.children.insert(
            "child".to_string(),
            test_child_record(
                "child",
                "child-session",
                "parent-session",
                &spec,
                SubagentStatus::Running,
                1,
                Some(Arc::clone(&nested)),
            ),
        );
        state.children.insert(
            "flat".to_string(),
            test_child_record("flat", "flat-session", "parent-session", &spec, SubagentStatus::Running, 1, None),
        );
    }

    let descendants = parent.descendant_controllers().await;

    assert_eq!(descendants.len(), 1, "only attached child scopes are reachable");
    assert!(Arc::ptr_eq(&descendants[0], &nested));

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn descendant_controllers_deduplicate_shared_child_scopes() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("parent controller");
    let spec = explorer_spec();
    let shared = nested_child_controller(temp.path().to_path_buf(), "first-child-session").await;

    {
        let mut state = parent.state.write().await;
        for id in ["first", "second"] {
            state.children.insert(
                id.to_string(),
                test_child_record(
                    id,
                    "first-child-session",
                    "parent-session",
                    &spec,
                    SubagentStatus::Running,
                    1,
                    Some(Arc::clone(&shared)),
                ),
            );
        }
    }

    assert_eq!(parent.descendant_controllers().await.len(), 1, "a shared child scope must be traversed once");

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn attach_child_controller_rejects_unknown_tasks() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("parent controller");
    let nested = nested_child_controller(temp.path().to_path_buf(), "child-session").await;

    let err = parent
        .attach_child_controller("missing", nested)
        .await
        .expect_err("attaching to an unknown task must fail closed");

    assert!(err.to_string().contains("Unknown subagent id missing"), "{err}");

    parent.signal_shutdown().await;
}

#[tokio::test]
async fn nested_controller_publishes_under_the_root_observer_scope() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("parent controller");
    let mut nested = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default());
        config.parent_session_id = "child-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("nested controller");
    nested.inherit_progress_observers(&parent).await;

    let root_observer = Arc::new(RecordingObserver::default());
    let child_observer = Arc::new(RecordingObserver::default());
    let _root_registration = parent.register_progress_observer("parent-session", root_observer.clone());
    let _child_registration = parent.register_progress_observer("child-session", child_observer.clone());

    let spec = explorer_spec();
    {
        let mut state = nested.state.write().await;
        state.children.insert(
            "grandchild".to_string(),
            test_child_record(
                "grandchild",
                "grandchild-session",
                "child-session",
                &spec,
                SubagentStatus::Running,
                2,
                None,
            ),
        );
    }

    nested.publish_child_status("grandchild").await;

    {
        let root_updates = root_observer.children.lock().expect("children lock");
        assert_eq!(root_updates.len(), 1, "root-scoped observers must see grandchild progress");
        assert_eq!(root_updates[0].id, "grandchild");
    }
    {
        let child_updates = child_observer.children.lock().expect("children lock");
        assert!(child_updates.is_empty(), "child-scoped keys must not receive aliased events");
    }

    // Shutdown publishes a terminal snapshot into the same observer, so the
    // assertions above must release the lock first.
    parent.signal_shutdown().await;
    nested.signal_shutdown().await;
}

#[tokio::test]
async fn nested_scope_is_inherited_through_more_than_one_level() {
    let temp = TempDir::new().expect("tempdir");
    let parent = SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
        .await
        .expect("parent controller");
    let mut first = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default());
        config.parent_session_id = "first-child-session".to_string();
        config.depth = 1;
        config
    })
    .await
    .expect("first nested controller");
    first.inherit_progress_observers(&parent).await;
    let mut second = SubagentController::new({
        let mut config = test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default());
        config.parent_session_id = "second-child-session".to_string();
        config.depth = 2;
        config
    })
    .await
    .expect("second nested controller");
    second.inherit_progress_observers(&first).await;

    let root_observer = Arc::new(RecordingObserver::default());
    let _registration = parent.register_progress_observer("parent-session", root_observer.clone());

    let spec = explorer_spec();
    {
        let mut state = second.state.write().await;
        state.children.insert(
            "great-grandchild".to_string(),
            test_child_record(
                "great-grandchild",
                "great-grandchild-session",
                "second-child-session",
                &spec,
                SubagentStatus::Running,
                3,
                None,
            ),
        );
    }

    second.publish_child_status("great-grandchild").await;

    {
        let updates = root_observer.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "every depth must publish under the root observer key");
        assert_eq!(updates[0].id, "great-grandchild");
    }

    // Shutdown publishes a terminal snapshot into the same observer, so the
    // assertion above must release the lock first.
    parent.signal_shutdown().await;
    first.signal_shutdown().await;
    second.signal_shutdown().await;
}

#[tokio::test]
async fn close_preserves_settled_foreground_outcomes() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
            .await
            .expect("controller");
    let spec = explorer_spec();

    for (id, status) in [
        ("completed", SubagentStatus::Completed),
        ("failed", SubagentStatus::Failed),
        ("closed", SubagentStatus::Closed),
    ] {
        let settled_at = Utc::now() - chrono::Duration::minutes(5);
        {
            let mut state = controller.state.write().await;
            let mut record = test_child_record(id, &format!("session-{id}"), "parent-session", &spec, status, 1, None);
            record.completed_at = Some(settled_at);
            record.error = (status == SubagentStatus::Failed).then(|| "provider unavailable".to_string());
            state.children.insert(id.to_string(), record);
        }

        let entry = controller.close(id).await.expect("close settled child");

        assert_eq!(entry.status, status, "closing {id} must preserve its settled outcome");
        assert_eq!(entry.completed_at, Some(settled_at), "closing {id} must preserve its completion time");
        if status == SubagentStatus::Failed {
            assert_eq!(entry.error.as_deref(), Some("provider unavailable"));
        }
    }

    // An active child is still cancellable.
    {
        let mut state = controller.state.write().await;
        state.children.insert(
            "active".to_string(),
            test_child_record("active", "session-active", "parent-session", &spec, SubagentStatus::Running, 1, None),
        );
    }
    let active = controller.close("active").await.expect("close active child");
    assert_eq!(active.status, SubagentStatus::Closed);

    controller.signal_shutdown().await;
}

#[tokio::test]
async fn cancel_preserves_settled_background_outcomes() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), VTCodeConfig::default()))
            .await
            .expect("controller");

    for (id, status, error) in [
        ("stopped", BackgroundSubprocessStatus::Stopped, None),
        ("errored", BackgroundSubprocessStatus::Error, Some("exited with status 3")),
    ] {
        let settled = settled_background_record(id, "parent-session", status, error);
        let settled_at = settled.ended_at.expect("settled timestamp");
        {
            let mut state = controller.state.write().await;
            state.background_children.insert(id.to_string(), settled);
        }

        let entry = controller
            .force_cancel_background(id)
            .await
            .expect("cancel settled background task");

        assert_eq!(entry.status, status, "cancelling {id} must preserve its settled outcome");
        assert_eq!(entry.ended_at, Some(settled_at), "cancelling {id} must preserve its end time");
        assert_eq!(entry.error.as_deref(), error, "cancelling {id} must preserve its error text");
    }

    controller.signal_shutdown().await;
}

/// A settled record keeps its outcome, but cancelling it still clears the
/// restore flag. Restore selection reads only `desired_enabled`, so a flag
/// left set would silently relaunch a task the owner had cancelled.
#[tokio::test]
async fn cancelling_a_settled_background_record_keeps_it_out_of_restore() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let mut vt_cfg = VTCodeConfig::default();
    vt_cfg.subagents.background.auto_restore = true;
    let controller = SubagentController::new(test_controller_config(workspace_root.clone(), vt_cfg.clone()))
        .await
        .expect("controller");

    let record_id = background_record_id_for_owner("watcher", Some("parent-session"));
    let mut settled = settled_background_record(
        &record_id,
        "parent-session",
        BackgroundSubprocessStatus::Error,
        Some("exited with status 3"),
    );
    settled.desired_enabled = true;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(record_id.clone(), settled);
    }

    let entry = controller
        .force_cancel_background(&record_id)
        .await
        .expect("cancel errored background task");
    assert_eq!(entry.status, BackgroundSubprocessStatus::Error, "the settled error must survive the cancel");
    assert_eq!(entry.error.as_deref(), Some("exited with status 3"));
    assert!(!entry.desired_enabled, "cancel must clear the restore flag");
    controller.signal_shutdown().await;

    let restored = SubagentController::new(test_controller_config(workspace_root, vt_cfg))
        .await
        .expect("restoring controller");
    let entries = restored
        .restore_background_subagents()
        .await
        .expect("restore background records");
    let record = entries
        .iter()
        .find(|entry| entry.id == record_id)
        .expect("the cancelled record stays in the ledger");
    assert!(!record.desired_enabled, "restoring must keep the cleared flag");
    assert!(!record.status.is_active(), "a cancelled record must not be relaunched on restore");
    restored.signal_shutdown().await;
}

#[tokio::test]
async fn nested_scope_never_restores_the_workspace_background_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), VTCodeConfig::default()))
        .await
        .expect("root controller");

    // Owner-scoped records use canonical owner-qualified ids, and the loader
    // rewrites any legacy plain id from `(agent_name, owner_session_id)`. Build
    // the fixture with the same helper so the persisted ledger round-trips with
    // the identity the restore path expects.
    let record_id = background_record_id_for_owner("watcher", Some("parent-session"));
    {
        let mut state = root.state.write().await;
        state.background_children.insert(
            record_id.clone(),
            settled_background_record(&record_id, "parent-session", BackgroundSubprocessStatus::Running, None),
        );
    }
    root.save_background_state().await.expect("persist root ledger");
    assert_eq!(root.background_status_entries().await.len(), 1);

    // A nested scope in the same workspace cannot own background work, so it
    // must not adopt the root's record as a second, stale copy.
    let nested = nested_child_controller(workspace_root.clone(), "child-session").await;
    assert!(
        nested.background_status_entries().await.is_empty(),
        "nested scopes must not restore records owned by another controller"
    );

    // Cancelling through the owner settles the one authoritative entry.
    let cancelled = root
        .force_cancel_background(&record_id)
        .await
        .expect("cancel root background task");
    assert_eq!(cancelled.status, BackgroundSubprocessStatus::Stopped);
    let root_entries = root.background_status_entries().await;
    assert_eq!(root_entries.len(), 1, "the root must report exactly one entry");
    assert_eq!(root_entries[0].status, BackgroundSubprocessStatus::Stopped);
    assert!(nested.background_status_entries().await.is_empty());

    // The nested scope's persistence guard must leave the ledger intact.
    nested.save_background_state().await.expect("nested save is a no-op");
    let restored = SubagentController::new(test_controller_config(workspace_root, VTCodeConfig::default()))
        .await
        .expect("restoring controller");
    let restored_entries = restored.background_status_entries().await;
    assert_eq!(restored_entries.len(), 1, "the workspace ledger must survive the nested scope");
    assert_eq!(restored_entries[0].id, record_id);
    assert_eq!(restored_entries[0].status, BackgroundSubprocessStatus::Stopped);

    nested.signal_shutdown().await;
    restored.signal_shutdown().await;
    root.signal_shutdown().await;
}
