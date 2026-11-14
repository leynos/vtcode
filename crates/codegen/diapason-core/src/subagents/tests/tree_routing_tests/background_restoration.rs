//! Background restoration: settled-outcome preservation and owner-scoped restore.
use super::*;

#[tokio::test]
async fn close_preserves_settled_foreground_outcomes() {
    let temp = TempDir::new().expect("tempdir");
    let controller =
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
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
        SubagentController::new(test_controller_config(temp.path().to_path_buf(), DiapasonConfig::default()))
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
    let mut vt_cfg = DiapasonConfig::default();
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
    assert_eq!(
        record.status,
        BackgroundSubprocessStatus::Error,
        "restore and its trailing refresh must preserve the settled error instead of relabelling it as a stopped exit"
    );
    assert_eq!(record.error.as_deref(), Some("exited with status 3"), "the settled error text must survive");
    restored.signal_shutdown().await;
}

/// The ACP workspace controller loads every record so a session controller can
/// be built from the same ledger, but it must never respawn work another
/// session owns. Only ownerless legacy records are eligible.
#[tokio::test]
async fn legacy_restore_ignores_records_owned_by_a_session() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let mut vt_cfg = DiapasonConfig::default();
    vt_cfg.subagents.background.auto_restore = true;
    let controller =
        SubagentController::new_with_background_owner(test_controller_config(workspace_root, vt_cfg), None)
            .await
            .expect("workspace controller");

    let owned_id = background_record_id_for_owner("watcher", Some("other-session"));
    let mut owned = settled_background_record(&owned_id, "other-session", BackgroundSubprocessStatus::Running, None);
    owned.desired_enabled = true;
    owned.exec_session_id = "missing-owned-session".to_string();
    let owned_updated_at = owned.updated_at;
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(owned_id.clone(), owned);
    }

    let restored = controller
        .restore_legacy_background_subagents()
        .await
        .expect("legacy restore must ignore a session-owned record");
    assert!(restored.is_empty(), "no ownerless record exists to restore");

    let untouched = controller
        .background_status_entries()
        .await
        .into_iter()
        .find(|entry| entry.id == owned_id)
        .expect("the owned record stays in the ledger");
    assert_eq!(untouched.status, BackgroundSubprocessStatus::Running);
    assert_eq!(
        untouched.updated_at, owned_updated_at,
        "an owned record must not be settled, respawned, or rewritten"
    );

    // Contrast: the unfiltered restore does select the same record, so the
    // exclusion above comes from the owner filter rather than an empty ledger.
    // The assertion names the record, because any other fault would also be an
    // error and would not prove the record was selected.
    let error = controller
        .restore_background_subagents()
        .await
        .expect_err("the unfiltered restore selects the owned record");
    assert!(
        format!("{error:#}").contains("Failed to resolve background subagent 'watcher'"),
        "the unfiltered restore must fail on the owned record, not on another fault: {error:#}"
    );

    controller.signal_shutdown().await;
}

#[tokio::test]
async fn nested_scope_never_restores_the_workspace_background_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
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
    let restored = SubagentController::new(test_controller_config(workspace_root, DiapasonConfig::default()))
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
