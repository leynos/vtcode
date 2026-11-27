//! Which observers a grafted controller's snapshots reach.
//!
//! A controller can be given a route before it is attached to the tree it ends
//! up in, so these cases pin that a graft neither drops a registration an
//! adapter already holds nor routes a record to another session's observers.
//! Declared by `mod observer_ownership;` from `tests.rs`.

use super::*;

/// A controller keeps the observers registered before it joined a tree.
///
/// A child-scoped controller can be registered with before it is grafted: the
/// adapter that owns the session registers on the controller it was handed, and
/// the graft then replaces the route with the tree's. Replacing the registry
/// outright would drop those registrations, so the adapter would silently stop
/// receiving snapshots for a session it never detached from. The registration
/// travels into the adopted registry instead, and its handle still detaches it
/// from the registry that now holds it.
#[tokio::test]
async fn a_route_swap_keeps_the_observers_registered_before_it() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let nested = SubagentController::new(nested_controller_config(workspace_root, "child-session"))
        .await
        .expect("nested controller");

    // The adapter registers against the nested controller before the graft.
    let observer = Arc::new(RecordingObserver::default());
    let registration = nested.register_progress_observer("parent-session", observer.clone());

    nested.inherit_progress_observers(&root, "parent-session").await;

    let record = child_record("grandchild", "parent-session", &explorer_spec(), SubagentStatus::Running, 2);
    nested.state.write().await.children.insert("grandchild".to_string(), record);
    nested.publish_child_status("grandchild").await;

    {
        let updates = observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "a registration made before the graft must keep receiving snapshots"
        );
    }

    // The handle detaches from the registry that now holds the registration,
    // so a later snapshot no longer reaches it.
    drop(registration);
    let later = child_record("later", "parent-session", &explorer_spec(), SubagentStatus::Running, 2);
    nested.state.write().await.children.insert("later".to_string(), later);
    nested.publish_child_status("later").await;
    {
        let updates = observer.children.lock().expect("children lock");
        assert_eq!(updates.len(), 1, "a detached registration must not receive later snapshots");
    }

    nested.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// A populated subtree publishing under another session is not grafted.
///
/// A controller that already owns records publishes them under the observer key
/// it adopted, and those records name the session that registered the adapter.
/// A graft rebinds the key to this tree's route, so a subtree owned by a
/// different session would deliver the older snapshots to this tree's observers
/// and to no one else. There is no way to re-key records after the fact, so the
/// attachment is refused before the route or the allocator is adopted, and the
/// existing link, route and records stay exactly as they were.
#[tokio::test]
async fn a_populated_subtree_owned_by_another_session_is_not_grafted() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let incoming = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "child-session"))
            .await
            .expect("incoming controller"),
    );
    let spec = explorer_spec();

    // The incoming controller already serves another session and holds a
    // record that publishes under it.
    incoming.inherit_progress_observers(&root, "other-session").await;
    let record = child_record("grandchild", "other-session", &spec, SubagentStatus::Running, 2);
    incoming.state.write().await.children.insert("grandchild".to_string(), record);
    let other_observer = Arc::new(RecordingObserver::default());
    let _other_registration = root.register_progress_observer("other-session", other_observer.clone());

    root.state
        .write()
        .await
        .children
        .insert("child".to_string(), child_record("child", "parent-session", &spec, SubagentStatus::Running, 1));

    let rejection = root
        .attach_child_controller("child", Arc::clone(&incoming))
        .await
        .expect_err("a subtree owned by another session must not be grafted into this one");
    assert!(
        format!("{rejection}").contains("publish under the other-session observer key"),
        "the rejection must name the owning session: {rejection}"
    );

    // The record and the incoming route are untouched: its snapshot still
    // reaches the observer registered for the session it already serves, and
    // nothing is published on the target tree's key.
    assert!(
        root.state
            .read()
            .await
            .children
            .get("child")
            .expect("the target record survives the rejection")
            .child_controller
            .is_none(),
        "a rejected graft must not publish the controller"
    );
    incoming.publish_child_status("grandchild").await;
    {
        let updates = other_observer.children.lock().expect("children lock");
        assert_eq!(
            updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
            vec!["grandchild"],
            "a rejected graft must leave the incoming route on the session it already serves"
        );
    }

    incoming.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// Renders the ledger row a background launch is persisted as.
///
/// The route a background snapshot takes is its record's parent session, so the
/// fixture names one explicitly rather than deriving it from the controller.
fn background_row(parent_session_id: &str) -> crate::subagents::PersistedBackgroundRecord {
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
    .unwrap_or_else(|error| panic!("background row fixture: {error}"))
}

/// A controller tracking another session is not grafted, even with no
/// inherited key.
///
/// An independently constructed controller resolves its observer key per
/// publication, so it holds no key while its foreground records already name
/// the session their snapshots belong to. Adopting the destination key would
/// send those snapshots to the wrong session's observers with no way to re-key
/// them afterwards.
#[tokio::test]
async fn a_controller_tracking_another_session_is_not_grafted() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let incoming = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "child-session"))
            .await
            .expect("incoming controller"),
    );
    let spec = explorer_spec();
    let record = child_record("grandchild", "other-session", &spec, SubagentStatus::Running, 2);
    incoming.state.write().await.children.insert("grandchild".to_string(), record);
    let other_observer = Arc::new(RecordingObserver::default());
    let _other_registration = incoming.register_progress_observer("other-session", other_observer.clone());
    root.state
        .write()
        .await
        .children
        .insert("child".to_string(), child_record("child", "parent-session", &spec, SubagentStatus::Running, 1));

    let rejection = root
        .attach_child_controller("child", Arc::clone(&incoming))
        .await
        .expect_err("a controller tracking another session must not be grafted");
    assert!(
        format!("{rejection}").contains("publish under the other-session observer key"),
        "the rejection must name the owning session: {rejection}"
    );

    incoming.publish_child_status("grandchild").await;
    let updates = other_observer.children.lock().expect("children lock").clone();
    assert_eq!(
        updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        vec!["grandchild"],
        "a rejected graft must leave the incoming route on the session its records name"
    );

    incoming.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// Background records count when the observer conflict is decided.
///
/// A controller can own only background launches: their snapshots route by each
/// record's parent session, not by any foreground child. Treating a controller
/// with no foreground children as empty let a graft adopt a route that would
/// re-point those snapshots at the destination session.
#[tokio::test]
async fn a_controller_whose_background_records_route_elsewhere_is_not_grafted() {
    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let root = SubagentController::new(test_controller_config(workspace_root.clone()))
        .await
        .expect("root controller");
    let incoming = Arc::new(
        SubagentController::new(nested_controller_config(workspace_root, "child-session"))
            .await
            .expect("incoming controller"),
    );
    let spec = explorer_spec();
    incoming.inherit_progress_observers(&root, "other-session").await;
    let background = crate::subagents::BackgroundRecord::from_persisted(background_row("other-session"));
    incoming
        .state
        .write()
        .await
        .background_children
        .insert(background.id.clone(), background);
    root.state
        .write()
        .await
        .children
        .insert("child".to_string(), child_record("child", "parent-session", &spec, SubagentStatus::Running, 1));

    let rejection = root
        .attach_child_controller("child", Arc::clone(&incoming))
        .await
        .expect_err("a background record routed elsewhere must not be re-keyed");
    assert!(
        format!("{rejection}").contains("publish under the other-session observer key"),
        "the rejection must name the routed session: {rejection}"
    );
    assert!(
        root.state
            .read()
            .await
            .children
            .get("child")
            .expect("the target record")
            .child_controller
            .is_none(),
        "a rejected graft must not publish the controller"
    );

    incoming.signal_shutdown().await;
    root.signal_shutdown().await;
}

/// A descendant's terminal update is delivered across the graft.
///
/// The graft moves the whole joining subtree onto one publication route. Where
/// each controller's route is rebound separately, a descendant that publishes
/// in the interval still names the registry its root's adoption has already
/// emptied: its terminal update finds no registration and is dropped while the
/// record settles, and the adapter keeps showing `Running`. The handoff holds
/// every route's write guard from the first swap to the last, so the
/// publication waits and is delivered afterwards.
///
/// The wait is observed through a standard channel, not by timing a runtime
/// task. The handoff parks one runtime worker on its checkpoint and the
/// publication parks another on the route guard, so a timer that needs a free
/// worker to fire could only report elapsed time after the checkpoint's own
/// bounded wait had already released the handoff. The leaf's route guard is
/// also probed directly while the graft is parked, which pins that the
/// checkpoint fires with every guard held rather than after the swaps.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_descendants_terminal_update_is_delivered_across_the_graft() {
    use crate::subagents::SubagentLockScope;
    use crate::subagents::controller_tree::attachment_checkpoints::{AttachmentStage, arm_blocking};
    use std::time::Duration;

    let temp = TempDir::new().expect("tempdir");
    let workspace_root = temp.path().to_path_buf();
    let scope = Arc::new(SubagentLockScope::new());
    let mut root_config = test_controller_config(workspace_root.clone());
    root_config.lock_scope = Some(Arc::clone(&scope));
    let root = Arc::new(SubagentController::new(root_config).await.expect("root controller"));
    let mut middle_config = nested_controller_config(workspace_root.clone(), "middle-session");
    middle_config.lock_scope = Some(Arc::clone(&scope));
    let middle = Arc::new(SubagentController::new(middle_config).await.expect("middle controller"));
    let leaf = {
        let mut leaf = SubagentController::new(nested_controller_config(workspace_root, "leaf-session"))
            .await
            .expect("leaf controller");
        // Production's only creator gives the leaf the middle controller's
        // registry, so before the graft the two share one route.
        leaf.inherit_tree_context(&middle, "middle-session").await;
        Arc::new(leaf)
    };
    let spec = explorer_spec();
    // Both records belong to the session that owns the whole run, which is the
    // tree the graft joins: only the routes they are published through differ.
    let mut leaf_record = child_record("leaf", "parent-session", &spec, SubagentStatus::Running, 2);
    leaf_record.child_controller = Some(Arc::clone(&leaf));
    middle.state.write().await.children.insert("leaf".to_string(), leaf_record);
    root.state
        .write()
        .await
        .children
        .insert("middle".to_string(), child_record("middle", "parent-session", &spec, SubagentStatus::Running, 1));
    leaf.state.write().await.children.insert(
        "leaf-task".to_string(),
        child_record("leaf-task", "parent-session", &spec, SubagentStatus::Running, 3),
    );

    // The registration lives in the joining subtree's registry, so it only
    // reaches the observer once the handoff has carried it into the tree's.
    let observer = Arc::new(RecordingObserver::default());
    let _registration = middle.register_progress_observer("parent-session", observer.clone());

    let (entered, release) = arm_blocking(&middle.state, AttachmentStage::DuringRouteHandoff);
    let graft = tokio::spawn({
        let root = Arc::clone(&root);
        let middle = Arc::clone(&middle);
        async move { root.attach_child_controller("middle", middle).await }
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the graft must reach the route handoff");

    // The checkpoint is inside the handoff's guard scope, so both the joining
    // controller's route and its descendant's must already be write-locked.
    // Probing directly does not depend on how the runtime schedules the
    // publication task below.
    assert!(
        !leaf.progress_route.write_guard_available(),
        "the handoff checkpoint must fire with the descendant's route guard held"
    );

    // The descendant publishes its terminal update while the handoff holds
    // every route guard. It must wait for the handoff instead of reading the
    // registry the root's adoption has already emptied. Completion is
    // signalled on a channel so the wait is measured without a timer that a
    // parked worker would have to drive.
    let (published, published_rx) = std::sync::mpsc::channel();
    let publication = tokio::spawn({
        let leaf = Arc::clone(&leaf);
        async move {
            leaf.publish_child_status("leaf-task").await;
            // Sent only once the publication has returned; while the handoff
            // holds the route guard this must not arrive.
            let _ = published.send(());
        }
    });
    let blocked = published_rx.recv_timeout(Duration::from_millis(100));
    assert!(
        matches!(blocked, Err(std::sync::mpsc::RecvTimeoutError::Timeout)),
        "a publication must wait for the whole route handoff"
    );

    release.send(()).expect("the graft is parked on the handoff checkpoint");
    tokio::time::timeout(Duration::from_secs(5), graft)
        .await
        .expect("the graft must finish once the handoff is released")
        .expect("the graft task must not panic")
        .expect("the graft publishes");
    tokio::time::timeout(Duration::from_secs(5), publication)
        .await
        .expect("the publication must finish once the handoff is released")
        .expect("the publication task must not panic");

    let updates = observer.children.lock().expect("children lock").clone();
    assert_eq!(
        updates.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(),
        vec!["leaf-task"],
        "a descendant's terminal update must reach the tree observers across the graft"
    );

    root.signal_shutdown().await;
    middle.signal_shutdown().await;
    leaf.signal_shutdown().await;
}
