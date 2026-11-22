//! Background restoration: settled-outcome preservation and owner-scoped restore.
use super::*;
use crate::subagents::background::{background_state_lock_path_for_tests, background_state_path_for_tests};

/// A managed-runtime controller must not create the workspace ledger at all.
///
/// Comparing the ledger's bytes before and after a nested save only proves the
/// save wrote the same content: a save that rewrote the file with identical
/// bytes would still pass. In a fresh workspace there is nothing to rewrite, so
/// the guard is checked by the absence of both files a real save creates — the
/// ledger and its lock sidecar.
#[tokio::test]
async fn a_managed_controller_never_creates_the_workspace_ledger() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let mut config = test_controller_config(workspace_root.clone(), DiapasonConfig::default());
    config.managed_background_runtime = true;
    let controller = SubagentController::new(config).await.expect("managed controller");

    controller.save_background_state().await.expect("a managed save is a no-op");

    assert!(
        !background_state_path_for_tests(&workspace_root).exists(),
        "a managed controller must not create the workspace ledger"
    );
    assert!(
        !background_state_lock_path_for_tests(&workspace_root).exists(),
        "a managed save must not even take the ledger lock"
    );
    controller.signal_shutdown().await;
}

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

        let entry = controller.close(TaskOwner::Workspace, id).await.expect("close settled child");

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
    let active = controller
        .close(TaskOwner::Workspace, "active")
        .await
        .expect("close active child");
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
            .force_cancel_background(TaskOwner::Workspace, id)
            .await
            .expect("cancel settled background task")
            .into_entry();

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
        .force_cancel_background(TaskOwner::Workspace, &record_id)
        .await
        .expect("cancel errored background task")
        .into_entry();
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

    let owned_id = background_record_id_for_owner("owned-watcher", Some("other-session"));
    let mut owned = settled_background_record(&owned_id, "other-session", BackgroundSubprocessStatus::Running, None);
    owned.agent_name = "owned-watcher".to_string();
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
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == owned_id)
        .expect("the owned record stays in the ledger");
    assert_eq!(untouched.status, BackgroundSubprocessStatus::Running);
    assert_eq!(
        untouched.updated_at, owned_updated_at,
        "an owned record must not be settled, respawned, or rewritten"
    );

    // Contrast: an ownerless record naming a different agent is selected by
    // the legacy restore, so the exclusion above comes from the owner filter
    // rather than an empty ledger. The assertion names the selected record, and
    // the two records carry different agent names, so a fault caused by the
    // owned record would not satisfy it.
    let legacy_id = background_record_id_for_owner("watcher", None);
    let mut legacy = settled_background_record(&legacy_id, "legacy-session", BackgroundSubprocessStatus::Running, None);
    legacy.owner_session_id = None;
    legacy.desired_enabled = true;
    legacy.exec_session_id = "missing-legacy-session".to_string();
    {
        let mut state = controller.state.write().await;
        state.background_children.insert(legacy_id.clone(), legacy);
    }

    let error = controller
        .restore_legacy_background_subagents()
        .await
        .expect_err("the legacy restore selects the ownerless record");
    assert!(
        format!("{error:#}").contains("Failed to resolve background subagent 'watcher'"),
        "the legacy restore must fail on the ownerless record, not on another fault: {error:#}"
    );

    let owned_after = controller
        .background_status_entries(TaskOwner::Workspace)
        .await
        .into_iter()
        .find(|entry| entry.id == owned_id)
        .expect("the owned record stays in the ledger");
    assert_eq!(
        owned_after.updated_at, owned_updated_at,
        "the failed ownerless restore must leave the owned record untouched"
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
    assert_eq!(root.background_status_entries(TaskOwner::Workspace).await.len(), 1);

    // A nested scope in the same workspace cannot own background work, so it
    // must not adopt the root's record as a second, stale copy.
    let nested = nested_child_controller(workspace_root.clone(), "child-session").await;
    assert!(
        nested.background_status_entries(TaskOwner::Workspace).await.is_empty(),
        "nested scopes must not restore records owned by another controller"
    );

    // Cancelling through the owner settles the one authoritative entry.
    let cancelled = root
        .force_cancel_background(TaskOwner::Workspace, &record_id)
        .await
        .expect("cancel root background task")
        .into_entry();
    assert_eq!(cancelled.status, BackgroundSubprocessStatus::Stopped);
    let root_entries = root.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(root_entries.len(), 1, "the root must report exactly one entry");
    assert_eq!(root_entries[0].status, BackgroundSubprocessStatus::Stopped);
    assert!(nested.background_status_entries(TaskOwner::Workspace).await.is_empty());

    // The nested scope's persistence guard must leave the ledger intact, not
    // merely preserve the records a round-trip would report: compare the raw
    // bytes so a save that rewrote the file with the same values also fails.
    let ledger_path = background_state_path_for_tests(&workspace_root);
    let ledger_before = std::fs::read(&ledger_path).expect("read the workspace ledger before the nested save");
    nested.save_background_state().await.expect("nested save is a no-op");
    assert_eq!(
        std::fs::read(&ledger_path).expect("read the workspace ledger after the nested save"),
        ledger_before,
        "a nested save must not rewrite the workspace ledger"
    );
    let restored = SubagentController::new(test_controller_config(workspace_root, DiapasonConfig::default()))
        .await
        .expect("restoring controller");
    let restored_entries = restored.background_status_entries(TaskOwner::Workspace).await;
    assert_eq!(restored_entries.len(), 1, "the workspace ledger must survive the nested scope");
    assert_eq!(restored_entries[0].id, record_id);
    assert_eq!(restored_entries[0].status, BackgroundSubprocessStatus::Stopped);

    nested.signal_shutdown().await;
    restored.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// An empty background owner names no owner.
///
/// `new_with_background_owner` is given the ACP session id, which is empty when
/// no session is attached. Keeping `Some("")` as a distinct owner would filter
/// the ledger down to records whose owner is the empty string, hiding every
/// real record, and would make the save merge treat ownerless legacy records as
/// foreign, while `background_record_id_for_owner` already maps an empty owner
/// to the base id. An empty owner must therefore behave exactly like no owner.
#[tokio::test]
async fn an_empty_background_owner_loads_like_an_unscoped_controller() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();

    // Persist a record owned by a real session through an unscoped controller.
    let writer = SubagentController::new(test_controller_config(workspace_root.clone(), DiapasonConfig::default()))
        .await
        .expect("writer controller");
    let record_id = background_record_id_for_owner("watcher", Some("other-session"));
    {
        let mut state = writer.state.write().await;
        state.background_children.insert(
            record_id.clone(),
            settled_background_record(&record_id, "other-session", BackgroundSubprocessStatus::Stopped, None),
        );
    }
    writer.save_background_state().await.expect("persist the ledger");
    writer.signal_shutdown().await;

    let empty_owner = SubagentController::new_with_background_owner(
        test_controller_config(workspace_root, DiapasonConfig::default()),
        Some(""),
    )
    .await
    .expect("empty-owner controller");
    assert!(
        empty_owner.background_owner_session_id.is_none(),
        "an empty owner must be dropped at construction rather than kept as a distinct owner"
    );
    let listed = empty_owner.background_status_entries(TaskOwner::Workspace).await;
    assert!(
        listed.iter().any(|entry| entry.id == record_id),
        "an empty owner must load the ledger like an unscoped controller: {listed:?}"
    );
    empty_owner.signal_shutdown().await;
}
